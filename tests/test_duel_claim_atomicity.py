"""Atomicity guards for boss-duel resolution and relic ownership.

These pin two hardening fixes:
- ``claim_active_duel`` must be an atomic read-and-delete so two concurrent
  ``resume_boss_duel`` calls can't both resolve the same paused duel (double
  payout / double gear+relic drop).
- ``equip_relic`` / ``unequip_relic`` must only touch a relic the
  ``(discord_id, guild_id)`` owner actually holds.
"""

from __future__ import annotations

import threading

from repositories.dig_repository import DigRepository
from tests.conftest import TEST_GUILD_ID


def _paused_duel_state() -> dict:
    return {
        "boss_id": "molemann", "tier": 1, "mechanic_id": "test_mech",
        "risk_tier": "bold", "wager": 50,
        "player_hp": 100, "boss_hp": 40, "round_num": 1,
        "rng_state": "",
        "player_hit": 0.9, "player_dmg": 30,
        "boss_hit": 0.5, "boss_dmg": 20,
    }


def test_claim_active_duel_consumes_row_exactly_once(repo_db_path):
    """Sequential contract: the first claim returns the row and deletes it,
    so a second (later) claim returns None and every reader sees it gone.
    The true concurrent race is covered by the threaded test below.
    """
    repo = DigRepository(repo_db_path)
    discord_id, guild_id = 77001, TEST_GUILD_ID
    repo.create_tunnel(discord_id, guild_id, "Vault")
    repo.save_active_duel(discord_id, guild_id, _paused_duel_state())

    first = repo.claim_active_duel(discord_id, guild_id)
    assert first is not None
    assert first["wager"] == 50
    assert first["boss_id"] == "molemann"

    second = repo.claim_active_duel(discord_id, guild_id)
    assert second is None, "second claim must be a no-op — the row is consumed"

    # The row is gone for every reader afterwards.
    assert repo.get_active_duel(discord_id, guild_id) is None


def test_concurrent_claims_exactly_one_wins(repo_db_path):
    """5 threads race ``claim_active_duel``; exactly one gets the row.

    This is the guarantee ``resume_boss_duel`` relies on: two concurrent
    modal submissions both pass the initial guard, but only the claim
    winner resolves the duel — the losers get None and bail, so the paused
    duel can never pay out (or drop gear/relics) twice.
    """
    repo = DigRepository(repo_db_path)
    discord_id, guild_id = 77002, TEST_GUILD_ID
    repo.create_tunnel(discord_id, guild_id, "Vault")
    repo.save_active_duel(discord_id, guild_id, _paused_duel_state())

    results: list[dict | None] = []
    lock = threading.Lock()
    barrier = threading.Barrier(5)

    def try_claim() -> None:
        barrier.wait()  # maximize overlap so the claims genuinely race
        row = repo.claim_active_duel(discord_id, guild_id)
        with lock:
            results.append(row)

    threads = [threading.Thread(target=try_claim) for _ in range(5)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    winners = [r for r in results if r is not None]
    assert len(winners) == 1, f"expected exactly 1 winning claim, got {len(winners)}"
    assert winners[0]["wager"] == 50
    assert winners[0]["boss_id"] == "molemann"
    assert repo.get_active_duel(discord_id, guild_id) is None


def test_equip_relic_is_scoped_to_owner(repo_db_path):
    """A foreign (discord_id, guild_id) can't flip another player's relic."""
    repo = DigRepository(repo_db_path)
    repo.create_tunnel(1, 0, "Owner")
    repo.create_tunnel(2, 0, "Other")
    owner_relic = int(repo.add_artifact(1, 0, "mole_claws", is_relic=True))

    # Player 2 forges player 1's artifact id — must not take effect.
    repo.equip_relic(owner_relic, 2, 0, True)
    art = next(a for a in repo.get_artifacts(1, 0) if int(a["id"]) == owner_relic)
    assert int(art["equipped"]) == 0, "cross-player equip must be a no-op"

    # The real owner equips it fine.
    repo.equip_relic(owner_relic, 1, 0, True)
    art = next(a for a in repo.get_artifacts(1, 0) if int(a["id"]) == owner_relic)
    assert int(art["equipped"]) == 1

    # A foreign unequip is likewise a no-op.
    repo.unequip_relic(owner_relic, 2, 0)
    art = next(a for a in repo.get_artifacts(1, 0) if int(a["id"]) == owner_relic)
    assert int(art["equipped"]) == 1, "cross-player unequip must be a no-op"
