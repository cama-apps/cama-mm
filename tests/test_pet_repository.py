"""Integration tests for PetRepository against a real schema-initialized DB."""

from __future__ import annotations

import sqlite3

import pytest

from domain.models.pet import RefundNotice, RefundPayout
from repositories.pet_repository import PetRepository
from tests.conftest import TEST_GUILD_ID

NOW = 1_800_000_000
HATCH = 86400
WEEK = "2026-W30"
TODAY = "2026-07-26"


@pytest.fixture
def pet_repo(repo_db_path):
    return PetRepository(repo_db_path)


@pytest.fixture
def rich_player(player_repository):
    player_repository.add(100, "Owner", TEST_GUILD_ID)
    player_repository.update_balance(100, TEST_GUILD_ID, 1000)
    return 100


def adopt(pet_repo, discord_id=100, guild_id=TEST_GUILD_ID, **overrides):
    resolved_species = overrides.pop("species", None)
    kwargs = {
        "name": "Blep",
        "egg_tier": "standard",
        "fee": 20,
        "now": NOW,
        "hatch_seconds": HATCH,
    }
    kwargs.update(overrides)
    egg = pet_repo.adopt_pet(discord_id, guild_id, **kwargs)
    if resolved_species is None:
        return egg
    return pet_repo.resolve_hatch_species(
        egg.pet_id,
        guild_id,
        species=resolved_species,
        now=egg.hatched_at,
    )


def stock(pet_repo, discord_id=100, guild_id=TEST_GUILD_ID, item="tango", qty=5):
    return pet_repo.buy_supplies(
        discord_id, guild_id, item_id=item, qty=qty, total_cost=qty * 5,
        now=NOW, stack_cap=99,
    )


def claim_death(pet_repo, pet, *, died_at):
    return pet_repo.claim_death(
        pet.pet_id, pet.guild_id, died_at=died_at,
        expected_last_fed_at=pet.last_fed_at,
        expected_hunger=pet.hunger_at_last_fed,
    )


def feed(pet_repo, pet, **overrides):
    kwargs = {
        "item_id": "tango",
        "expected_last_fed_at": pet.last_fed_at,
        "expected_hunger": pet.hunger_at_last_fed,
        "new_last_fed_at": NOW + HATCH + 100,
        "new_hunger": 100,
        "feed_date": TODAY,
        "feed_cap": 4,
        "week_key": WEEK,
        "consumed_jc": 5,
        "spat": False,
    }
    kwargs.update(overrides)
    return pet_repo.feed_pet(pet.pet_id, pet.discord_id, pet.guild_id, **kwargs)


def _ledger_rows(db_path, source):
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    try:
        return conn.execute(
            "SELECT * FROM economy_ledger_entries WHERE source = ?", (source,)
        ).fetchall()
    finally:
        conn.close()


def _seed_nonprofit(db_path, guild_id, amount):
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
            "INSERT INTO nonprofit_fund (guild_id, total_collected) VALUES (?, ?) "
            "ON CONFLICT(guild_id) DO UPDATE SET total_collected = excluded.total_collected",
            (guild_id, amount),
        )
        conn.commit()
    finally:
        conn.close()


class TestAdopt:
    def test_adopt_debits_fee_and_creates_egg(
        self, pet_repo, player_repository, rich_player
    ):
        pet = adopt(pet_repo)
        assert pet.hatched_at == NOW + HATCH
        assert pet.hunger_at_last_fed == 100
        assert pet.last_fed_at == pet.hatched_at
        assert pet.species == "unhatched"
        assert pet.egg_tier == "standard"
        assert pet.dig_work_units == 0
        assert pet.dig_work_at == pet.hatched_at
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 980
        assert len(_ledger_rows(pet_repo.db_path, "pet")) == 1

    def test_adopt_persists_gilded_roll_pool_without_selecting_species(
        self, pet_repo, rich_player
    ):
        pet = adopt(pet_repo, egg_tier="gilded", fee=250)
        assert pet.species == "unhatched"
        assert pet.egg_tier == "gilded"

    def test_adopt_insufficient_funds_leaves_no_row_and_no_debit(
        self, pet_repo, player_repository
    ):
        player_repository.add(101, "Poor", TEST_GUILD_ID)
        player_repository.update_balance(101, TEST_GUILD_ID, 10)
        with pytest.raises(ValueError, match="insufficient_funds"):
            adopt(pet_repo, discord_id=101)
        assert pet_repo.get_active_pet(101, TEST_GUILD_ID) is None
        assert player_repository.get_balance(101, TEST_GUILD_ID) == 10

    def test_adopt_with_living_pet_rejected_and_not_charged(
        self, pet_repo, player_repository, rich_player
    ):
        adopt(pet_repo)
        with pytest.raises(ValueError, match="already_has_pet"):
            adopt(pet_repo)
        # Only the first fee was taken (second transaction rolled back).
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 980

    def test_dead_pet_does_not_block_re_adopt(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        assert claim_death(pet_repo, pet, died_at=NOW + 9 * 86400)
        second = adopt(pet_repo, name="Blep II")
        assert second.pet_id != pet.pet_id
        assert pet_repo.count_dead_pets(100, TEST_GUILD_ID) == 1

    def test_guild_isolation_and_none_normalization(
        self, pet_repo, player_repository, rich_player
    ):
        player_repository.add(100, "Owner", 0)
        player_repository.update_balance(100, 0, 500)
        adopt(pet_repo)
        pet_none = adopt(pet_repo, guild_id=None, name="DMcama")
        assert pet_none.guild_id == 0
        assert pet_repo.get_active_pet(100, None).pet_id == pet_none.pet_id
        assert pet_repo.get_active_pet(100, 0).pet_id == pet_none.pet_id
        assert pet_repo.get_active_pet(100, TEST_GUILD_ID).name == "Blep"


class TestEggUpgradeAndHatch:
    def test_upgrade_debits_premium_and_changes_only_roll_pool(
        self, pet_repo, player_repository, rich_player
    ):
        egg = adopt(pet_repo)
        upgraded = pet_repo.upgrade_egg(
            100,
            TEST_GUILD_ID,
            name="Goldie",
            premium=230,
            now=NOW + 1,
        )
        assert upgraded.pet_id == egg.pet_id
        assert upgraded.name == "Goldie"
        assert upgraded.hatched_at == egg.hatched_at
        assert upgraded.species == "unhatched"
        assert upgraded.egg_tier == "gilded"
        assert upgraded.adopt_fee == 250
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 750
        assert len(_ledger_rows(pet_repo.db_path, "pet")) == 2

    def test_upgrade_failure_rolls_back_state_and_payment(
        self, pet_repo, player_repository, rich_player
    ):
        egg = adopt(pet_repo)
        player_repository.update_balance(100, TEST_GUILD_ID, 100)
        with pytest.raises(ValueError, match="insufficient_funds"):
            pet_repo.upgrade_egg(
                100,
                TEST_GUILD_ID,
                name="Too Expensive",
                premium=230,
                now=NOW + 1,
            )
        unchanged = pet_repo.get_pet_by_id(egg.pet_id, TEST_GUILD_ID)
        assert unchanged.name == "Blep"
        assert unchanged.egg_tier == "standard"
        assert unchanged.adopt_fee == 20
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 100

    def test_upgrade_rejects_gilded_or_hatched_egg(
        self, pet_repo, player_repository, rich_player
    ):
        gilded = adopt(pet_repo, egg_tier="gilded", fee=250)
        with pytest.raises(ValueError, match="already_gilded"):
            pet_repo.upgrade_egg(
                100,
                TEST_GUILD_ID,
                name="Again",
                premium=230,
                now=NOW + 1,
            )
        assert claim_death(pet_repo, gilded, died_at=NOW + 2)
        standard = adopt(pet_repo, name="Second")
        with pytest.raises(ValueError, match="pet_hatched"):
            pet_repo.upgrade_egg(
                100,
                TEST_GUILD_ID,
                name="Too Late",
                premium=230,
                now=standard.hatched_at,
            )
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 730

    def test_hatch_resolution_is_ready_gated_and_write_once(
        self, pet_repo, rich_player
    ):
        egg = adopt(pet_repo)
        with pytest.raises(ValueError, match="egg_not_ready"):
            pet_repo.resolve_hatch_species(
                egg.pet_id,
                TEST_GUILD_ID,
                species="rama",
                now=egg.hatched_at - 1,
            )
        resolved = pet_repo.resolve_hatch_species(
            egg.pet_id,
            TEST_GUILD_ID,
            species="rama",
            now=egg.hatched_at,
        )
        assert resolved.species == "rama"
        retried = pet_repo.resolve_hatch_species(
            egg.pet_id,
            TEST_GUILD_ID,
            species="common_cama",
            now=egg.hatched_at + 1,
        )
        assert retried.species == "rama"


class TestSupplies:
    def test_buy_upserts_and_debits(self, pet_repo, player_repository, rich_player):
        assert stock(pet_repo, qty=3) == 3
        assert stock(pet_repo, qty=2) == 5
        assert pet_repo.get_supplies(100, TEST_GUILD_ID) == {"tango": 5}
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 1000 - 25

    def test_buy_respects_stack_cap_and_rolls_back_debit(
        self, pet_repo, player_repository, rich_player
    ):
        stock(pet_repo, qty=5)
        with pytest.raises(ValueError, match="stack_cap"):
            pet_repo.buy_supplies(
                100, TEST_GUILD_ID, item_id="tango", qty=95, total_cost=475,
                now=NOW, stack_cap=99,
            )
        assert pet_repo.get_supplies(100, TEST_GUILD_ID) == {"tango": 5}
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 975

    def test_buy_insufficient_funds(self, pet_repo, player_repository):
        player_repository.add(102, "Broke", TEST_GUILD_ID)
        player_repository.update_balance(102, TEST_GUILD_ID, 3)
        with pytest.raises(ValueError, match="insufficient_funds"):
            stock(pet_repo, discord_id=102, qty=1)
        assert pet_repo.get_supplies(102, TEST_GUILD_ID) == {}


class TestFeed:
    def test_feed_decrements_stock_and_moves_anchor(
        self, pet_repo, rich_player
    ):
        pet = adopt(pet_repo)
        stock(pet_repo)
        fed = feed(pet_repo, pet, new_hunger=90)
        assert fed.hunger_at_last_fed == 90
        assert fed.last_fed_at == NOW + HATCH + 100
        assert fed.times_fed == 1
        assert fed.feeds_today == 1
        assert fed.feed_date == TODAY
        assert fed.week_consumed_jc == 5
        assert fed.week_key == WEEK
        assert pet_repo.get_supplies(100, TEST_GUILD_ID) == {"tango": 4}

    def test_feed_without_stock_changes_nothing(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        with pytest.raises(ValueError, match="no_supplies"):
            feed(pet_repo, pet)
        fresh = pet_repo.get_active_pet(100, TEST_GUILD_ID)
        assert fresh.last_fed_at == pet.last_fed_at
        assert fresh.times_fed == 0

    def test_feed_dead_pet_restores_stock(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        stock(pet_repo)
        claim_death(pet_repo, pet, died_at=NOW + 9 * 86400)
        with pytest.raises(ValueError, match="pet_dead"):
            feed(pet_repo, pet)
        assert pet_repo.get_supplies(100, TEST_GUILD_ID) == {"tango": 5}

    def test_feed_cap_enforced_and_resets_across_game_days(
        self, pet_repo, rich_player
    ):
        pet = adopt(pet_repo)
        stock(pet_repo, qty=10)
        current = pet
        for i in range(4):
            current = feed(
                pet_repo, current,
                new_last_fed_at=NOW + HATCH + 100 + i, new_hunger=100,
            )
        with pytest.raises(ValueError, match="feed_cap"):
            feed(pet_repo, current, new_last_fed_at=NOW + HATCH + 200)
        # A new game-date resets the counter.
        after = feed(
            pet_repo, current,
            feed_date="2026-07-27", new_last_fed_at=NOW + HATCH + 300,
        )
        assert after.feeds_today == 1
        assert after.feed_date == "2026-07-27"

    def test_week_accumulator_rolls_over(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        stock(pet_repo, qty=5)
        first = feed(pet_repo, pet, consumed_jc=5)
        second = feed(
            pet_repo, first, expected_last_fed_at=first.last_fed_at,
            expected_hunger=first.hunger_at_last_fed,
            new_last_fed_at=first.last_fed_at + 10,
            week_key="2026-W31", consumed_jc=5,
        )
        assert first.week_consumed_jc == 5
        assert second.week_consumed_jc == 5  # reset, not 10
        assert second.week_key == "2026-W31"
        # Last week's care shifted into the prev slot for the refund sweep.
        assert second.prev_week_key == WEEK
        assert second.prev_week_consumed_jc == 5
        assert second.consumed_in_week(WEEK) == 5
        # And the refund sweep still finds the pet for last week.
        assert [p.pet_id for p in pet_repo.list_week_care(TEST_GUILD_ID, WEEK)] == [
            second.pet_id
        ]

    def test_stale_anchor_rejected(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        stock(pet_repo)
        feed(pet_repo, pet)
        with pytest.raises(ValueError, match="stale_pet"):
            feed(pet_repo, pet)  # stale expected anchors

    def test_same_second_pet_work_change_rejects_stale_feed(
        self, pet_repo, rich_player
    ):
        pet = adopt(pet_repo)
        stock(pet_repo)
        with pet_repo.connection() as conn:
            conn.execute(
                "UPDATE pets SET dig_work_units = ? WHERE pet_id = ?",
                (86400, pet.pet_id),
            )

        with pytest.raises(ValueError, match="stale_pet"):
            feed(
                pet_repo,
                pet,
                expected_dig_work_units=pet.dig_work_units,
                expected_dig_work_at=pet.dig_work_at,
                new_dig_work_units=0,
                new_dig_work_at=pet.dig_work_at,
            )

    def test_spat_feed_consumes_item_but_not_hunger(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        stock(pet_repo)
        spat = feed(pet_repo, pet, spat=True, consumed_jc=5)
        assert spat.hunger_at_last_fed == pet.hunger_at_last_fed
        assert spat.last_fed_at == pet.last_fed_at
        assert spat.times_fed == 0
        assert spat.feeds_today == 1  # still burns a feed action
        assert spat.week_consumed_jc == 5  # still counts as care spend
        assert pet_repo.get_supplies(100, TEST_GUILD_ID) == {"tango": 4}


class TestTrinkets:
    def test_dead_pet_is_never_charged_for_a_trinket(
        self, pet_repo, player_repository, rich_player
    ):
        """Regression: liveness re-checked INSIDE the transaction, so a death
        claimed after the caller's check can't be debited."""
        pet = adopt(pet_repo)
        claim_death(pet_repo, pet, died_at=NOW + 9 * 86400)
        with pytest.raises(ValueError, match="pet_dead"):
            pet_repo.buy_trinket_atomic(
                pet.pet_id, 100, TEST_GUILD_ID,
                accessory_id="top_hat", cost=25, refund=10, now=NOW,
            )
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 980  # fee only
        assert pet_repo.get_accessories(100, TEST_GUILD_ID) == []

    def test_trinket_roll_debits_equips_and_dupe_refunds(
        self, pet_repo, player_repository, rich_player
    ):
        pet = adopt(pet_repo)
        outcome = pet_repo.buy_trinket_atomic(
            pet.pet_id, 100, TEST_GUILD_ID,
            accessory_id="top_hat", cost=25, refund=10, now=NOW,
        )
        assert outcome == {"duplicate": False, "net_cost": 25, "owned_count": 1}
        assert pet_repo.get_active_pet(100, TEST_GUILD_ID).accessory == "top_hat"
        dupe = pet_repo.buy_trinket_atomic(
            pet.pet_id, 100, TEST_GUILD_ID,
            accessory_id="top_hat", cost=25, refund=10, now=NOW + 1,
        )
        assert dupe == {"duplicate": True, "net_cost": 15, "owned_count": 1}
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 980 - 25 - 15

    def test_equip_raises_distinct_codes(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        with pytest.raises(ValueError, match="not_owned"):
            pet_repo.equip_accessory(pet.pet_id, 100, TEST_GUILD_ID, "red_bow")
        pet_repo.buy_trinket_atomic(
            pet.pet_id, 100, TEST_GUILD_ID,
            accessory_id="red_bow", cost=25, refund=10, now=NOW,
        )
        pet_repo.equip_accessory(pet.pet_id, 100, TEST_GUILD_ID, "red_bow")
        claim_death(pet_repo, pet, died_at=NOW + 9 * 86400)
        with pytest.raises(ValueError, match="pet_dead"):
            pet_repo.equip_accessory(pet.pet_id, 100, TEST_GUILD_ID, "red_bow")


class TestDeathAndRevival:
    def test_claim_death_is_once_only(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        assert claim_death(pet_repo, pet, died_at=NOW + 777)
        assert not claim_death(pet_repo, pet, died_at=NOW + 999)
        dead = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert dead.died_at == NOW + 777
        assert dead.death_cause == "starvation"

    def test_aegis_revive_fires_exactly_once(self, pet_repo, rich_player):
        pet = adopt(pet_repo, species="aegis_cama")
        assert pet_repo.revive_with_aegis(
            pet.pet_id, TEST_GUILD_ID,
            revived_last_fed_at=NOW + 6 * 86400, revive_hunger=10,
            expected_last_fed_at=pet.last_fed_at,
            expected_hunger=pet.hunger_at_last_fed,
        )
        revived = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert revived.aegis_used == 1
        assert revived.hunger_at_last_fed == 10
        assert not pet_repo.revive_with_aegis(
            pet.pet_id, TEST_GUILD_ID,
            revived_last_fed_at=NOW + 7 * 86400, revive_hunger=10,
            expected_last_fed_at=revived.last_fed_at,
            expected_hunger=revived.hunger_at_last_fed,
        )

    def test_starvation_candidates_boundary(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        # common_cama at 20/day: starves 5 days after the (hatch) anchor.
        death = pet.last_fed_at + 5 * 86400
        early = pet_repo.find_starvation_candidates(
            death - 86400, decay_per_day=20, max_decay_pct=110
        )
        # The superset scan may include not-quite-starved pets near the end
        # (it uses the fastest decay), but a pet a full day out is excluded.
        assert all(p.pet_id != pet.pet_id for p in early)
        due = pet_repo.find_starvation_candidates(
            death, decay_per_day=20, max_decay_pct=110
        )
        assert any(p.pet_id == pet.pet_id for p in due)

    def test_stale_anchor_death_claim_loses_to_a_feed(self, pet_repo, rich_player):
        """Regression: the sweep must not kill a pet fed after its snapshot."""
        pet = adopt(pet_repo)
        stock(pet_repo)
        feed(pet_repo, pet, new_hunger=100)  # anchors move
        # A claim computed from the PRE-feed snapshot must lose.
        assert not pet_repo.claim_death(
            pet.pet_id, TEST_GUILD_ID, died_at=NOW + 5 * 86400,
            expected_last_fed_at=pet.last_fed_at,
            expected_hunger=pet.hunger_at_last_fed,
        )
        assert pet_repo.get_active_pet(100, TEST_GUILD_ID) is not None

    def test_stale_anchor_aegis_revive_loses_to_a_feed(self, pet_repo, rich_player):
        pet = adopt(pet_repo, species="aegis_cama")
        stock(pet_repo)
        fed = feed(pet_repo, pet, new_hunger=100)
        assert not pet_repo.revive_with_aegis(
            pet.pet_id, TEST_GUILD_ID,
            revived_last_fed_at=NOW + 5 * 86400, revive_hunger=10,
            expected_last_fed_at=pet.last_fed_at,
            expected_hunger=pet.hunger_at_last_fed,
        )
        fresh = pet_repo.get_pet_by_id(pet.pet_id, TEST_GUILD_ID)
        assert fresh.aegis_used == 0  # Aegis not wasted
        assert fresh.last_fed_at == fed.last_fed_at  # feed preserved

    def test_unannounced_death_flow(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        claim_death(pet_repo, pet, died_at=NOW + 500)
        pending = pet_repo.get_unannounced_deaths()
        assert [p.pet_id for p in pending] == [pet.pet_id]
        pet_repo.mark_death_announced(pet.pet_id, TEST_GUILD_ID, NOW + 600)
        assert pet_repo.get_unannounced_deaths() == []

    def test_unannounced_hatch_flow(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        assert pet_repo.find_unannounced_hatches(NOW) == []  # still an egg
        ready = pet_repo.find_unannounced_hatches(NOW + HATCH)
        assert [p.pet_id for p in ready] == [pet.pet_id]
        pet_repo.mark_hatch_announced(pet.pet_id, TEST_GUILD_ID, NOW + HATCH + 1)
        assert pet_repo.find_unannounced_hatches(NOW + HATCH) == []


class TestSaltLickAndRename:
    def test_salt_lick_sets_pampered_and_counts_care(
        self, pet_repo, player_repository, rich_player
    ):
        pet = adopt(pet_repo)
        fresh = pet_repo.use_salt_lick(
            pet.pet_id, 100, TEST_GUILD_ID,
            cost=8, now=NOW + HATCH, pampered_until=NOW + HATCH + 43200,
            week_key=WEEK,
        )
        assert fresh.pampered_until == NOW + HATCH + 43200
        assert fresh.week_consumed_jc == 8
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 972
        with pytest.raises(ValueError, match="already_pampered"):
            pet_repo.use_salt_lick(
                pet.pet_id, 100, TEST_GUILD_ID,
                cost=8, now=NOW + HATCH + 100,
                pampered_until=NOW + HATCH + 43300, week_key=WEEK,
            )

    def test_rename_debits_and_updates(
        self, pet_repo, player_repository, rich_player
    ):
        pet = adopt(pet_repo)
        pet_repo.rename_pet(
            pet.pet_id, 100, TEST_GUILD_ID, new_name="Sir Blep", cost=10
        )
        assert pet_repo.get_active_pet(100, TEST_GUILD_ID).name == "Sir Blep"
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 970


class TestRefunds:
    def test_refund_notice_persists_until_marked(
        self, pet_repo, player_repository, rich_player
    ):
        _seed_nonprofit(pet_repo.db_path, TEST_GUILD_ID, 500)
        notice = RefundNotice(
            guild_id=TEST_GUILD_ID,
            week_key=WEEK,
            payouts=(
                RefundPayout(
                    discord_id=100,
                    consumed_jc=20,
                    multiplier_pct=200,
                    amount=40,
                ),
            ),
            total_paid=40,
            scaled_down=False,
        )

        assert pet_repo.pay_refunds_atomic(
            TEST_GUILD_ID,
            WEEK,
            [(100, 40)],
            NOW,
            announcement=notice,
        )
        assert pet_repo.get_unannounced_refunds() == [notice]

        pet_repo.mark_refund_announced(TEST_GUILD_ID, WEEK, NOW + 1)
        assert pet_repo.get_unannounced_refunds() == []

    def test_pay_refunds_claims_window_once(
        self, pet_repo, player_repository, rich_player
    ):
        _seed_nonprofit(pet_repo.db_path, TEST_GUILD_ID, 500)
        paid = pet_repo.pay_refunds_atomic(
            TEST_GUILD_ID, WEEK, [(100, 40)], NOW
        )
        assert paid
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 1040
        assert pet_repo.get_nonprofit_balance(TEST_GUILD_ID) == 460
        # Second attempt is a no-op.
        assert not pet_repo.pay_refunds_atomic(TEST_GUILD_ID, WEEK, [(100, 40)], NOW)
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 1040
        assert pet_repo.is_refund_window_paid(TEST_GUILD_ID, WEEK)
        # Fund debit + player credit, both attributed to pet_refund.
        assert len(_ledger_rows(pet_repo.db_path, "pet_refund")) == 2

    def test_fund_short_rolls_back_claim(
        self, pet_repo, player_repository, rich_player
    ):
        _seed_nonprofit(pet_repo.db_path, TEST_GUILD_ID, 10)
        with pytest.raises(ValueError, match="fund_short"):
            pet_repo.pay_refunds_atomic(TEST_GUILD_ID, WEEK, [(100, 40)], NOW)
        assert not pet_repo.is_refund_window_paid(TEST_GUILD_ID, WEEK)
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 1000

    def test_missing_player_share_returns_to_fund(
        self, pet_repo, player_repository, rich_player
    ):
        """Regression: an unpayable share must not be destroyed."""
        _seed_nonprofit(pet_repo.db_path, TEST_GUILD_ID, 500)
        paid = pet_repo.pay_refunds_atomic(
            TEST_GUILD_ID, WEEK, [(100, 40), (999999, 60)], NOW
        )
        assert paid
        assert player_repository.get_balance(100, TEST_GUILD_ID) == 1040
        # The ghost's 60 went back to the fund: 500 - 100 + 60 = 460.
        assert pet_repo.get_nonprofit_balance(TEST_GUILD_ID) == 460

    def test_refund_ledger_covers_fund_debit(self, pet_repo, rich_player):
        _seed_nonprofit(pet_repo.db_path, TEST_GUILD_ID, 500)
        pet_repo.pay_refunds_atomic(TEST_GUILD_ID, WEEK, [(100, 40)], NOW)
        rows = _ledger_rows(pet_repo.db_path, "pet_refund")
        assert rows, "fund debit and credits must carry pet_refund attribution"

    def test_zero_total_claims_without_fund(self, pet_repo, rich_player):
        assert pet_repo.pay_refunds_atomic(TEST_GUILD_ID, WEEK, [], NOW)
        assert pet_repo.is_refund_window_paid(TEST_GUILD_ID, WEEK)

    def test_week_care_listing(self, pet_repo, rich_player):
        pet = adopt(pet_repo)
        stock(pet_repo)
        feed(pet_repo, pet)
        assert [p.pet_id for p in pet_repo.list_week_care(TEST_GUILD_ID, WEEK)] == [
            pet.pet_id
        ]
        assert pet_repo.list_week_care(TEST_GUILD_ID, "2026-W99") == []
        assert pet_repo.list_pet_guild_ids() == [TEST_GUILD_ID]
