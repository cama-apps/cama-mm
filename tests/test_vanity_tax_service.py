from concurrent.futures import ThreadPoolExecutor
from threading import Barrier, Event
from time import sleep
from types import SimpleNamespace

import pytest

from infrastructure.schema_manager import SchemaManager
from repositories.tax_repository import TaxRepository
from services.vanity_tax_service import VanityTaxService

GUILD_ID = 123


def _member(
    discord_id: int,
    nickname: str | None,
    *,
    global_name: str | None = None,
):
    return SimpleNamespace(
        id=discord_id,
        nick=nickname,
        global_name=global_name,
    )


def test_refresh_taxes_only_members_without_server_nicknames():
    service = VanityTaxService()

    service.refresh_guild(
        GUILD_ID,
        [
            _member(1, None),
            _member(2, "Real Name"),
        ],
    )

    assert service.calculate_tax(1, GUILD_ID, 199) == 19  # 10% floored
    assert service.calculate_tax(2, GUILD_ID, 199) == 0


def test_global_display_name_does_not_exempt_without_server_nickname():
    service = VanityTaxService()

    service.refresh_guild(
        GUILD_ID,
        [_member(1, None, global_name="ANI")],
    )

    assert service.calculate_tax(1, GUILD_ID, 100) == 10


def test_member_updates_toggle_taxability_and_removal_fails_open():
    service = VanityTaxService()
    service.refresh_guild(GUILD_ID, [_member(1, None)])

    service.update_member(GUILD_ID, 1, "Real Name")
    assert service.calculate_tax(1, GUILD_ID, 500) == 0

    service.update_member(GUILD_ID, 1, None)
    assert service.calculate_tax(1, GUILD_ID, 500) == 50

    service.remove_member(GUILD_ID, 1)
    assert service.calculate_tax(1, GUILD_ID, 500) == 0


def test_failed_enforcement_refresh_keeps_guild_eligibility_unknown():
    class FailingRepository:
        def get_vanity_tax_enforcements(self, guild_id):
            raise RuntimeError("database unavailable")

    service = VanityTaxService(FailingRepository())

    with pytest.raises(RuntimeError, match="database unavailable"):
        service.refresh_guild(GUILD_ID, [_member(1, None)])

    assert service.taxable_ids(GUILD_ID) == frozenset()
    assert service.eligibility_status(GUILD_ID, 1) == "unknown"


def test_manual_taxation_persists_and_overrides_server_nickname(repo_db_path):
    repository = TaxRepository(repo_db_path)
    service = VanityTaxService(repository)
    service.refresh_guild(GUILD_ID, [_member(1, "Skater")])

    assert service.calculate_tax(1, GUILD_ID, 500) == 0

    service.set_manual_taxation(GUILD_ID, 1, enforced=True, actor_id=99)
    assert service.is_manually_taxed(GUILD_ID, 1) is True
    assert service.calculate_tax(1, GUILD_ID, 500) == 50

    reloaded = VanityTaxService(repository)
    reloaded.refresh_guild(GUILD_ID, [_member(1, "Skater")])
    assert reloaded.is_manually_taxed(GUILD_ID, 1) is True
    assert reloaded.calculate_tax(1, GUILD_ID, 500) == 50
    reloaded.refresh_guild(GUILD_ID + 1, [_member(1, "Skater")])
    assert reloaded.calculate_tax(1, GUILD_ID + 1, 500) == 0

    reloaded.set_manual_taxation(GUILD_ID, 1, enforced=False, actor_id=100)
    assert reloaded.is_manually_taxed(GUILD_ID, 1) is False
    assert reloaded.calculate_tax(1, GUILD_ID, 500) == 0

    after_clear = VanityTaxService(repository)
    after_clear.refresh_guild(GUILD_ID, [_member(1, "Skater")])
    assert after_clear.is_manually_taxed(GUILD_ID, 1) is False
    assert after_clear.calculate_tax(1, GUILD_ID, 500) == 0


def test_enforcement_migration_does_not_reinterpret_legacy_exemptions(
    repo_db_path,
):
    repository = TaxRepository(repo_db_path)
    with repository.connection() as conn:
        conn.execute("DROP TABLE vanity_tax_enforcements")
        conn.execute(
            "DELETE FROM schema_migrations WHERE name = ?",
            ("create_vanity_tax_enforcements",),
        )
        conn.execute(
            """
            INSERT INTO vanity_tax_exemptions (
                guild_id, discord_id, exempted_by, created_at
            )
            VALUES (?, ?, ?, ?)
            """,
            (GUILD_ID, 1, 99, 1),
        )

    SchemaManager(repo_db_path).initialize()

    with repository.connection() as conn:
        legacy_row = conn.execute(
            """
            SELECT exempted_by
            FROM vanity_tax_exemptions
            WHERE guild_id = ? AND discord_id = ?
            """,
            (GUILD_ID, 1),
        ).fetchone()
        enforcement_row = conn.execute(
            """
            SELECT enforced_by
            FROM vanity_tax_enforcements
            WHERE guild_id = ? AND discord_id = ?
            """,
            (GUILD_ID, 1),
        ).fetchone()

    assert legacy_row["exempted_by"] == 99
    assert enforcement_row is None


def test_committed_manual_taxation_updates_cache_without_repository_reload():
    class Repository:
        def __init__(self):
            self.enforcements = set()
            self.read_count = 0

        def get_vanity_tax_enforcements(self, guild_id):
            self.read_count += 1
            if self.read_count > 1:
                raise RuntimeError("reload failed")
            return frozenset(self.enforcements)

        def set_vanity_tax_enforcement(
            self,
            guild_id,
            discord_id,
            *,
            enforced,
            actor_id,
        ):
            if enforced:
                self.enforcements.add(discord_id)
            else:
                self.enforcements.discard(discord_id)
            return frozenset(self.enforcements)

    repository = Repository()
    service = VanityTaxService(repository)
    service.refresh_guild(GUILD_ID, [_member(1, "Skater")])

    service.set_manual_taxation(GUILD_ID, 1, enforced=True, actor_id=99)

    assert repository.enforcements == {1}
    assert service.eligibility_status(GUILD_ID, 1) == "manual_taxation"
    assert service.calculate_tax(1, GUILD_ID, 500) == 50


def test_eligibility_status_distinguishes_every_cached_state():
    service = VanityTaxService()
    service.refresh_guild(
        GUILD_ID,
        [_member(1, None), _member(2, "Real Name")],
    )

    assert service.eligibility_status(GUILD_ID, 1) == "taxable"
    assert service.eligibility_status(GUILD_ID, 2) == "nickname_exemption"
    assert service.eligibility_status(GUILD_ID, 3) == "unknown"

    service.set_manual_taxation(GUILD_ID, 2, enforced=True, actor_id=99)
    assert service.eligibility_status(GUILD_ID, 2) == "manual_taxation"

    service.update_member(GUILD_ID, 3, None)
    assert service.eligibility_status(GUILD_ID, 3) == "taxable"
    service.remove_member(GUILD_ID, 3)
    assert service.eligibility_status(GUILD_ID, 3) == "unknown"


def test_concurrent_manual_taxations_do_not_lose_cached_members():
    class SlowCache(dict):
        def get(self, key, default=None):
            value = super().get(key, default)
            sleep(0.05)
            return value

    service = VanityTaxService()
    service.refresh_guild(
        GUILD_ID,
        [_member(1, "Skater"), _member(2, "Spooky Bush")],
    )
    service._manual_taxable_by_guild = SlowCache(
        {GUILD_ID: frozenset()}
    )
    start = Barrier(2)

    def enforce(discord_id: int) -> None:
        start.wait()
        service.set_manual_taxation(
            GUILD_ID,
            discord_id,
            enforced=True,
            actor_id=99,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = [executor.submit(enforce, discord_id) for discord_id in (1, 2)]
        for future in futures:
            future.result()

    assert service.taxable_ids(GUILD_ID) == frozenset({1, 2})


def test_concurrent_persisted_taxations_do_not_lose_members(repo_db_path):
    repository = TaxRepository(repo_db_path)
    service = VanityTaxService(repository)
    service.refresh_guild(
        GUILD_ID,
        [_member(1, "Skater"), _member(2, "Spooky Bush")],
    )
    start = Barrier(2)

    def enforce(discord_id: int) -> None:
        start.wait()
        service.set_manual_taxation(
            GUILD_ID,
            discord_id,
            enforced=True,
            actor_id=99,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = [executor.submit(enforce, discord_id) for discord_id in (1, 2)]
        for future in futures:
            future.result()

    assert service.taxable_ids(GUILD_ID) == frozenset({1, 2})
    reloaded = VanityTaxService(repository)
    reloaded.refresh_guild(
        GUILD_ID,
        [_member(1, "Skater"), _member(2, "Spooky Bush")],
    )
    assert reloaded.taxable_ids(GUILD_ID) == frozenset({1, 2})


def test_enforcement_started_during_refresh_wins_cache_publication():
    class InterleavingRepository:
        def __init__(self):
            self.enforcements: set[int] = set()
            self.refresh_read_started = Event()
            self.release_refresh_read = Event()

        def get_vanity_tax_enforcements(self, guild_id):
            self.refresh_read_started.set()
            assert self.release_refresh_read.wait(timeout=1)
            return frozenset(self.enforcements)

        def set_vanity_tax_enforcement(
            self, guild_id, discord_id, *, enforced, actor_id
        ):
            if enforced:
                self.enforcements.add(discord_id)
            else:
                self.enforcements.discard(discord_id)
            return frozenset(self.enforcements)

    repository = InterleavingRepository()
    service = VanityTaxService(repository)
    write_started = Event()

    def enforce() -> None:
        write_started.set()
        service.set_manual_taxation(
            GUILD_ID,
            1,
            enforced=True,
            actor_id=99,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        refresh = executor.submit(
            service.refresh_guild,
            GUILD_ID,
            [_member(1, "Skater")],
        )
        assert repository.refresh_read_started.wait(timeout=1)
        write = executor.submit(enforce)
        assert write_started.wait(timeout=1)
        repository.release_refresh_read.set()
        refresh.result()
        write.result()

    assert repository.enforcements == {1}
    assert service.eligibility_status(GUILD_ID, 1) == "manual_taxation"
    assert service.calculate_tax(1, GUILD_ID, 500) == 50


def _cache_lock_free_from_other_thread(service) -> bool:
    """True when ``service._lock`` can be acquired by a different thread."""
    result: dict[str, bool] = {}

    def probe() -> None:
        acquired = service._lock.acquire(timeout=1.0)
        result["acquired"] = acquired
        if acquired:
            service._lock.release()

    with ThreadPoolExecutor(max_workers=1) as executor:
        executor.submit(probe).result()
    return result.get("acquired", False)


def test_repository_io_runs_outside_shared_cache_lock():
    """DB reads/writes must not hold the cache lock the event loop acquires.

    ``refresh_guild`` (called from on_ready) and ``set_manual_taxation``
    (called from /tax vanity) perform SQLite I/O; holding ``_lock`` across it
    would block the sync member-event handlers running on the event loop.
    """
    observations: list[bool] = []

    class ProbingRepository:
        def __init__(self):
            self.service = None

        def get_vanity_tax_enforcements(self, guild_id):
            observations.append(_cache_lock_free_from_other_thread(self.service))
            return frozenset()

        def set_vanity_tax_enforcement(
            self, guild_id, discord_id, *, enforced, actor_id
        ):
            observations.append(_cache_lock_free_from_other_thread(self.service))
            return frozenset({discord_id} if enforced else set())

    repository = ProbingRepository()
    service = VanityTaxService(repository)
    repository.service = service

    service.refresh_guild(GUILD_ID, [_member(1, "Skater")])
    service.set_manual_taxation(GUILD_ID, 1, enforced=True, actor_id=99)

    assert observations == [True, True]
    # The cache still ends up consistent after both operations.
    assert service.eligibility_status(GUILD_ID, 1) == "manual_taxation"


def test_refresh_does_not_clobber_member_events_landing_mid_refresh():
    """Member events racing an off-loop refresh must win over the snapshot.

    ``refresh_guild`` now runs in a worker thread from a snapshot built on the
    event loop; ``update_member``/``remove_member`` firing between snapshot and
    store used to be wholesale-overwritten by the stale snapshot (re-taxing a
    freshly nicknamed member, resurrecting a departed one). The repository
    read sits exactly in that window, so the stub fires the events from it.
    """

    class InterleavingRepository:
        def __init__(self):
            self.service = None
            self.calls = 0

        def get_vanity_tax_enforcements(self, guild_id):
            self.calls += 1
            if self.calls == 1:
                # Simulate event-loop handlers firing mid-refresh.
                self.service.update_member(guild_id, 1, "Real Name")
                self.service.remove_member(guild_id, 2)
                self.service.update_member(guild_id, 3, None)
            return frozenset()

    repository = InterleavingRepository()
    service = VanityTaxService(repository)
    repository.service = service

    service.refresh_guild(GUILD_ID, [_member(1, None), _member(2, None)])

    assert service.eligibility_status(GUILD_ID, 1) == "nickname_exemption"
    assert service.eligibility_status(GUILD_ID, 2) == "unknown"
    assert service.eligibility_status(GUILD_ID, 3) == "taxable"
    assert service.taxable_ids(GUILD_ID) == frozenset({3})

    # The event journal is consumed by the store: an undisturbed refresh
    # applies its snapshot cleanly again.
    service.refresh_guild(GUILD_ID, [_member(1, None), _member(2, None)])
    assert service.taxable_ids(GUILD_ID) == frozenset({1, 2})
    assert service.eligibility_status(GUILD_ID, 3) == "unknown"


def test_begin_refresh_lets_the_fresh_snapshot_repair_missed_events():
    """The on_ready resync must win over journal entries older than its
    snapshot: events recorded before ``begin_refresh`` reflect pre-outage
    state and must not be re-applied over the fresh member list."""
    service = VanityTaxService()
    service.refresh_guild(GUILD_ID, [_member(1, None), _member(2, None)])
    # Pre-outage events land in the journal...
    service.update_member(GUILD_ID, 1, None)
    service.update_member(GUILD_ID, 2, None)
    # ...then the gateway reconnects: while disconnected, member 1 set a
    # nickname and member 2 left. Snapshot authority begins here.
    service.begin_refresh(GUILD_ID)
    service.refresh_guild(GUILD_ID, [_member(1, "Real Name")])

    assert service.eligibility_status(GUILD_ID, 1) == "nickname_exemption"
    assert service.eligibility_status(GUILD_ID, 2) == "unknown"
    assert service.taxable_ids(GUILD_ID) == frozenset()


def test_superseded_refresh_generation_does_not_clobber_the_newer_store():
    """When resyncs overlap, the older snapshot's store must be dropped: it
    would otherwise consume the journal and overwrite the newer state."""
    service = VanityTaxService()
    gen1 = service.begin_refresh(GUILD_ID)
    # A second resync begins while the first is still off-loop...
    gen2 = service.begin_refresh(GUILD_ID)
    # ...and a member event lands after the second snapshot was taken.
    service.update_member(GUILD_ID, 1, "Real Name")

    # The stale worker finishes first: its store is discarded and the
    # journal survives for the current generation.
    service.refresh_guild(GUILD_ID, [_member(1, None)], generation=gen1)
    assert service.taxable_ids(GUILD_ID) == frozenset()

    # The current worker stores its (pre-event) snapshot: the journaled
    # event is re-applied on top, so the nickname still exempts member 1.
    service.refresh_guild(GUILD_ID, [_member(1, None)], generation=gen2)
    assert service.eligibility_status(GUILD_ID, 1) == "nickname_exemption"
    assert service.taxable_ids(GUILD_ID) == frozenset()


def test_tax_floors_ten_percent_and_ignores_unknown_or_nonpositive_profit():
    service = VanityTaxService()
    service.refresh_guild(GUILD_ID, [_member(1, None)])

    # Floor keeps tiny profits (< 10 JC) untaxed.
    assert service.calculate_tax(1, GUILD_ID, 9) == 0
    assert service.calculate_tax(1, GUILD_ID, 99) == 9
    assert service.calculate_tax(1, GUILD_ID, 100) == 10
    assert service.calculate_tax(1, GUILD_ID, 0) == 0
    assert service.calculate_tax(1, GUILD_ID, -100) == 0
    assert service.calculate_tax(999, GUILD_ID, 1_000) == 0
    assert service.calculate_tax(1, 999, 1_000) == 0


def test_settlement_announcements_show_vanity_tax():
    from commands.match import MatchCommands
    from commands.predictions import _build_resolution_announcement_chunks

    match_text = MatchCommands._format_bet_distribution(
        None,
        [{"discord_id": 1, "payout": 200, "amount": 100}],
        [],
        {},
        {1: 1},
    )
    assert "won 199" in match_text
    assert "−1" in match_text
    assert "vanity tax" in match_text

    prediction_text = "\n".join(
        _build_resolution_announcement_chunks(
            5,
            "yes",
            [
                {
                    "discord_id": 1,
                    "cost_basis": 100,
                    "yes_contracts": 2,
                    "no_contracts": 0,
                    "payout": 199,
                    "profit": 99,
                }
            ],
            0,
            1,
        )
    )
    assert "1 JC vanity tax" in prediction_text
