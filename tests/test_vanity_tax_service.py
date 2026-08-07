from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from time import sleep
from types import SimpleNamespace

import pytest

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


def test_failed_exemption_refresh_keeps_guild_eligibility_unknown():
    class FailingRepository:
        def get_vanity_tax_exemptions(self, guild_id):
            raise RuntimeError("database unavailable")

    service = VanityTaxService(FailingRepository())

    with pytest.raises(RuntimeError, match="database unavailable"):
        service.refresh_guild(GUILD_ID, [_member(1, None)])

    assert service.taxable_ids(GUILD_ID) == frozenset()
    assert service.eligibility_status(GUILD_ID, 1) == "unknown"


def test_manual_exemption_persists_and_overrides_missing_nickname(repo_db_path):
    repository = TaxRepository(repo_db_path)
    service = VanityTaxService(repository)
    service.refresh_guild(GUILD_ID, [_member(1, None)])

    assert service.calculate_tax(1, GUILD_ID, 500) == 50

    service.set_manual_exemption(GUILD_ID, 1, exempt=True, actor_id=99)
    assert service.is_manually_exempt(GUILD_ID, 1) is True
    assert service.calculate_tax(1, GUILD_ID, 500) == 0

    reloaded = VanityTaxService(repository)
    reloaded.refresh_guild(GUILD_ID, [_member(1, None)])
    assert reloaded.is_manually_exempt(GUILD_ID, 1) is True
    assert reloaded.calculate_tax(1, GUILD_ID, 500) == 0
    reloaded.refresh_guild(GUILD_ID + 1, [_member(1, None)])
    assert reloaded.calculate_tax(1, GUILD_ID + 1, 500) == 50

    reloaded.set_manual_exemption(GUILD_ID, 1, exempt=False, actor_id=100)
    assert reloaded.is_manually_exempt(GUILD_ID, 1) is False
    assert reloaded.calculate_tax(1, GUILD_ID, 500) == 50

    after_revoke = VanityTaxService(repository)
    after_revoke.refresh_guild(GUILD_ID, [_member(1, None)])
    assert after_revoke.is_manually_exempt(GUILD_ID, 1) is False
    assert after_revoke.calculate_tax(1, GUILD_ID, 500) == 50


def test_committed_manual_exemption_updates_cache_without_repository_reload():
    class Repository:
        def __init__(self):
            self.exemptions = set()
            self.read_count = 0

        def get_vanity_tax_exemptions(self, guild_id):
            self.read_count += 1
            if self.read_count > 1:
                raise RuntimeError("reload failed")
            return frozenset(self.exemptions)

        def set_vanity_tax_exemption(
            self,
            guild_id,
            discord_id,
            *,
            exempt,
            actor_id,
        ):
            if exempt:
                self.exemptions.add(discord_id)
            else:
                self.exemptions.discard(discord_id)
            return frozenset(self.exemptions)

    repository = Repository()
    service = VanityTaxService(repository)
    service.refresh_guild(GUILD_ID, [_member(1, None)])

    service.set_manual_exemption(GUILD_ID, 1, exempt=True, actor_id=99)

    assert repository.exemptions == {1}
    assert service.eligibility_status(GUILD_ID, 1) == "manual_exemption"
    assert service.calculate_tax(1, GUILD_ID, 500) == 0


def test_eligibility_status_distinguishes_every_cached_state():
    service = VanityTaxService()
    service.refresh_guild(
        GUILD_ID,
        [_member(1, None), _member(2, "Real Name")],
    )

    assert service.eligibility_status(GUILD_ID, 1) == "taxable"
    assert service.eligibility_status(GUILD_ID, 2) == "nickname_exemption"
    assert service.eligibility_status(GUILD_ID, 3) == "unknown"

    service.set_manual_exemption(GUILD_ID, 1, exempt=True, actor_id=99)
    assert service.eligibility_status(GUILD_ID, 1) == "manual_exemption"

    service.update_member(GUILD_ID, 3, None)
    assert service.eligibility_status(GUILD_ID, 3) == "taxable"
    service.remove_member(GUILD_ID, 3)
    assert service.eligibility_status(GUILD_ID, 3) == "unknown"


def test_concurrent_manual_exemptions_do_not_lose_cached_members():
    class SlowCache(dict):
        def get(self, key, default=None):
            value = super().get(key, default)
            sleep(0.05)
            return value

    service = VanityTaxService()
    service.refresh_guild(GUILD_ID, [_member(1, None), _member(2, None)])
    service._manual_exemptions_by_guild = SlowCache(
        {GUILD_ID: frozenset()}
    )
    start = Barrier(2)

    def exempt(discord_id: int) -> None:
        start.wait()
        service.set_manual_exemption(
            GUILD_ID,
            discord_id,
            exempt=True,
            actor_id=99,
        )

    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = [executor.submit(exempt, discord_id) for discord_id in (1, 2)]
        for future in futures:
            future.result()

    assert service.taxable_ids(GUILD_ID) == frozenset()


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

    ``refresh_guild`` (called from on_ready) and ``set_manual_exemption``
    (called from /tax vanity) perform SQLite I/O; holding ``_lock`` across it
    would block the sync member-event handlers running on the event loop.
    """
    observations: list[bool] = []

    class ProbingRepository:
        def __init__(self):
            self.service = None

        def get_vanity_tax_exemptions(self, guild_id):
            observations.append(_cache_lock_free_from_other_thread(self.service))
            return frozenset()

        def set_vanity_tax_exemption(self, guild_id, discord_id, *, exempt, actor_id):
            observations.append(_cache_lock_free_from_other_thread(self.service))
            return frozenset({discord_id} if exempt else set())

    repository = ProbingRepository()
    service = VanityTaxService(repository)
    repository.service = service

    service.refresh_guild(GUILD_ID, [_member(1, None)])
    service.set_manual_exemption(GUILD_ID, 1, exempt=True, actor_id=99)

    assert observations == [True, True]
    # The cache still ends up consistent after both operations.
    assert service.eligibility_status(GUILD_ID, 1) == "manual_exemption"


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

        def get_vanity_tax_exemptions(self, guild_id):
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
