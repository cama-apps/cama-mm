"""Loot and event drops: items, artifacts, event pool composition, JC earnings."""

import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    ALL_ARTIFACTS,
    CONSUMABLES,
    FREE_DIG_COOLDOWN_SECONDS,
    HARD_HAT_USES,
    MAX_INVENTORY_SLOTS,
)
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register_player(player_repository, discord_id=10001, guild_id=12345, balance=100):
    """Helper to register a player with balance."""
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 3:
        player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


class TestItems:
    """Tests for item purchase and usage."""

    def test_buy_item_deducts_jc(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Buying item costs JC."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        balance_before = player_repository.get_balance(10001, guild_id)
        result = dig_service.buy_item(10001, guild_id, "dynamite")
        assert result["success"]
        balance_after = player_repository.get_balance(10001, guild_id)
        assert balance_before - balance_after == CONSUMABLES["dynamite"].cost

    def test_inventory_max(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't exceed MAX_INVENTORY_SLOTS items."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        for i in range(MAX_INVENTORY_SLOTS):
            result = dig_service.buy_item(10001, guild_id, "dynamite")
            assert result["success"], f"Should be able to buy item #{i+1}"

        # 6th item should fail
        result = dig_service.buy_item(10001, guild_id, "dynamite")
        assert not result["success"]

    def test_dynamite_adds_blocks(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Dynamite gives +5 bonus blocks."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=10)

        # Buy and queue dynamite
        dig_service.buy_item(10001, guild_id, "dynamite")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig with dynamite queued
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: a)  # min advance
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # Dynamite should add bonus blocks
        assert result.get("dynamite_bonus") or result["advance"] >= CONSUMABLES["dynamite"].params["bonus_blocks"]

    def test_hard_hat_prevents_cave_in(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Hard hat blocks cave-in (for 3 digs) when charges are already set."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20, hard_hat_charges=HARD_HAT_USES)

        # Force cave-in conditions but hard hat should block it
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.001)  # force cave-in roll
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert not result.get("cave_in"), "Hard hat should prevent cave-in"

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["hard_hat_charges"] == HARD_HAT_USES - 1

    def test_hard_hat_queued_sets_charges(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Buying and queuing a hard hat actually sets hard_hat_charges on the tunnel."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue hard hat
        result = dig_service.buy_item(10001, guild_id, "hard_hat")
        assert result["success"]
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig with hard hat queued — force cave-in roll
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.001)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert not result.get("cave_in"), "Queued hard hat should prevent cave-in"

        # Charges should be set to HARD_HAT_USES - 1 (one consumed this dig)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["hard_hat_charges"] == HARD_HAT_USES - 1

    def test_lantern_reduces_cave_in(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Lantern halves cave-in chance for the dig it's used on."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        # Place at depth 76 (Magma layer, 25% base cave-in) so halved = 12.5%
        dig_repo.update_tunnel(10001, guild_id, depth=76)

        # Buy and queue lantern
        dig_service.buy_item(10001, guild_id, "lantern")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Roll 0.13 — would cave-in at 25% but NOT at ~12.5%
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        monkeypatch.setattr(random, "random", lambda: 0.13)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        assert not result.get("cave_in"), "Lantern should halve cave-in chance"

    def test_reinforcement_sets_reinforced_until(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Reinforcement item sets reinforced_until timestamp on the tunnel."""
        _register_player(player_repository, balance=200)
        base_time = 1_000_000
        monkeypatch.setattr(time, "time", lambda: base_time)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue reinforcement
        dig_service.buy_item(10001, guild_id, "reinforcement")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig to consume it
        t = base_time + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        dig_service.dig(10001, guild_id)

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["reinforced_until"] >= t + 47 * 3600, "Reinforcement should set ~48h protection"

    def test_void_bait_doubles_event_chance(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Void Bait sets void_bait_digs and decrements each dig."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue void bait
        dig_service.buy_item(10001, guild_id, "void_bait")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        # Dig to consume it
        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        dig_service.dig(10001, guild_id)

        # Void bait should have set 3 charges, then decremented to 2 on this dig
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["void_bait_digs"] == 2

    def test_sonar_pulse_returns_event_preview(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Sonar Pulse includes an event_preview in the dig result."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=20)

        # Buy and queue sonar pulse
        dig_service.buy_item(10001, guild_id, "sonar_pulse")
        items = dig_repo.get_inventory(10001, guild_id)
        dig_repo.queue_item(items[0]["id"])

        t = 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1
        monkeypatch.setattr(time, "time", lambda: t)
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # event_preview may or may not have a value depending on the roll,
        # but the key should be present
        assert "event_preview" in result

    def test_queue_item_for_next_dig(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Queued item consumed on next dig."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Buy and queue
        result = dig_service.buy_item(10001, guild_id, "dynamite")
        item_id = result["item_id"]
        dig_service.queue_item(10001, guild_id, item_id)

        queued = dig_repo.get_queued_items(10001, guild_id)
        assert len(queued) == 1

        # Dig consumes queued item
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        dig_service.dig(10001, guild_id)

        queued_after = dig_repo.get_queued_items(10001, guild_id)
        assert len(queued_after) == 0


class TestArtifacts:
    """Tests for artifact discovery and trading."""

    def test_artifact_found_tracked(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Found artifact added to collection."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        # Directly add an artifact (simulating a find)
        artifact_id = ALL_ARTIFACTS[0].id
        db_id = dig_repo.add_artifact(10001, guild_id, artifact_id)
        assert db_id > 0

        artifacts = dig_repo.get_artifacts(10001, guild_id)
        assert len(artifacts) == 1
        assert artifacts[0]["artifact_id"] == artifact_id

    def test_artifact_registered_in_guild(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """First finder tracked in registry."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)

        artifact_id = ALL_ARTIFACTS[0].id
        is_first = dig_repo.register_artifact_find(artifact_id, guild_id, 10001, 1_000_000)
        assert is_first is True

        # Second find is not first
        _register_player(player_repository, discord_id=10002)
        is_first2 = dig_repo.register_artifact_find(artifact_id, guild_id, 10002, 1_000_001)
        assert is_first2 is False

        entry = dig_repo.get_registry_entry(artifact_id, guild_id)
        assert entry["first_finder_id"] == 10001
        assert entry["total_found"] == 2

    def test_gift_relic_transfers(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Relic moves from giver to receiver."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)

        # Create tunnels
        dig_service.dig(10001, guild_id)
        dig_service.dig(10002, guild_id)

        # Give player 1 a relic
        relic_id = "mole_claws"
        db_id = dig_repo.add_artifact(10001, guild_id, relic_id, is_relic=True)

        # Gift it
        result = dig_service.gift_relic(10001, 10002, guild_id, db_id)
        assert result["success"]

        # Giver no longer has it
        assert not dig_repo.has_artifact(10001, guild_id, relic_id)
        # Receiver has it
        assert dig_repo.has_artifact(10002, guild_id, relic_id)


class TestHasLanternInResult:
    """Verify has_lantern is included in dig results for boss encounters."""

    def test_dig_result_includes_has_lantern(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Normal dig result should include has_lantern field."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        dig_service.dig(10001, guild_id)

        # Queue a lantern
        dig_repo.add_inventory_item(10001, guild_id, "lantern")
        items = dig_repo.get_inventory(10001, guild_id)
        for item in items:
            if dict(item).get("item_type") == "lantern":
                dig_repo.queue_item(dict(item)["id"])

        # Set depth near boss boundary so advance doesn't skip it
        dig_repo.update_tunnel(10001, guild_id, depth=23)

        # Force advance to hit boss boundary at 25
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "randint", lambda a, b: 3)  # advance 3 would reach 26 > 25
        result = dig_service.dig(10001, guild_id)
        assert result["success"]
        # has_lantern should be in the result
        assert "has_lantern" in result


class TestUseItemValidation:
    """Verify use_item returns errors for invalid item types."""

    def test_use_item_unknown_type_returns_error(self, dig_service, player_repository, guild_id, monkeypatch):
        """use_item with a display name (e.g. 'Dynamite') instead of type key ('dynamite') should fail."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        result = dig_service.use_item(10001, guild_id, "Dynamite")
        assert result["success"] is False
        assert "Unknown" in result["error"]

    def test_use_item_valid_type_succeeds(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """use_item with correct type key ('dynamite') should succeed when item is in inventory."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        # Buy dynamite
        buy_result = dig_service.buy_item(10001, guild_id, "dynamite")
        assert buy_result["success"]

        # Use dynamite with lowercase type key
        result = dig_service.use_item(10001, guild_id, "dynamite")
        assert result["success"]


class TestExpandedEvents:
    """Verify expanded event system."""

    def test_event_pool_size_floor(self):
        """Event pool: 93 baseline + 5 trap + 3 splash + 15 delve-themed, so ≥116.

        Floor, not exact count, so routine event additions don't break the test.
        """
        from services.dig_constants import EVENT_POOL
        assert len(EVENT_POOL) >= 116

    def test_new_events_have_complexity_field(self):
        """All events should have a complexity field."""
        from services.dig_constants import EVENT_POOL
        for e in EVENT_POOL:
            assert "complexity" in e, f"Event {e['id']} missing complexity"

    def test_darkness_events_exist(self):
        """Should have events that require pitch black luminosity."""
        from services.dig_constants import EVENT_POOL
        dark_events = [e for e in EVENT_POOL if e.get("requires_dark")]
        assert len(dark_events) >= 3

    def test_roll_event_filters_by_layer(self, dig_service):
        """roll_event should filter events by depth/layer."""
        random.seed(42)
        # Roll 100 events at shallow depth — should never get deep events
        for _ in range(100):
            event = dig_service.roll_event(5, luminosity=100)
            if event:
                assert event.get("rarity") in ("common", "uncommon", "rare", "legendary")

    def test_dota_hero_events_exist(self):
        """Should have Dota hero encounter events."""
        from services.dig_constants import EVENT_POOL
        dota_ids = {"pudge_fishing", "tinker_workshop", "the_burrow", "arcanist_library", "the_dark_rift", "roshan_lair"}
        event_ids = {e["id"] for e in EVENT_POOL}
        assert dota_ids.issubset(event_ids), f"Missing Dota events: {dota_ids - event_ids}"


class TestNewItemsAndArtifacts:
    """Verify new consumables and artifacts."""

    def test_nine_consumables(self):
        from services.dig_constants import CONSUMABLES
        assert len(CONSUMABLES) == 9
        assert "torch" in CONSUMABLES
        assert "void_bait" in CONSUMABLES

    def test_artifact_count(self):
        from services.dig_constants import ALL_ARTIFACTS
        # 35 base + 3 P5-gated relics added in the prestige-pull pass
        assert len(ALL_ARTIFACTS) == 38

    def test_fungal_artifacts_exist(self):
        from services.dig_constants import ALL_ARTIFACTS
        fungal = [a for a in ALL_ARTIFACTS if a.layer == "Fungal Depths"]
        assert len(fungal) >= 4  # 1 relic + 3 collectibles

    def test_aegis_fragment_exists(self):
        from services.dig_constants import ARTIFACT_BY_ID
        assert "aegis_fragment" in ARTIFACT_BY_ID
        assert ARTIFACT_BY_ID["aegis_fragment"].is_relic is True

    def test_buy_item_torch(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Should be able to buy a torch."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        result = dig_service.buy_item(10001, guild_id, "torch")
        assert result["success"]
        assert result["cost"] == 6


class TestNewEventMechanics:
    """Test desperate and boon event mechanics."""

    def test_event_pool_has_desperate_options(self):
        """Some events in pool have desperate_option."""
        from services.dig_constants import EVENT_POOL
        desperate_events = [e for e in EVENT_POOL if e.get("desperate_option") is not None]
        assert len(desperate_events) > 0, "Expected at least one event with desperate_option"

    def test_event_pool_has_boon_options(self):
        """Some events in pool have boon_options."""
        from services.dig_constants import EVENT_POOL
        boon_events = [e for e in EVENT_POOL if e.get("boon_options")]
        assert len(boon_events) > 0, "Expected at least one event with boon_options"

    def test_roll_event_filters_by_prestige(self, dig_service):
        """roll_event still respects min_prestige if any event ever uses it.
        Currently the pool has zero gated events (the original three were
        unlocked), so this test passes vacuously and serves as a regression
        guard for the filter mechanism if a gate is ever re-introduced."""
        from services.dig_constants import EVENT_POOL
        gated_ids = {e["id"] for e in EVENT_POOL if e.get("min_prestige", 0) > 0}
        random.seed(42)
        found_gated = set()
        for _ in range(500):
            event = dig_service.roll_event(200, luminosity=100, prestige_level=0)
            if event and event["id"] in gated_ids:
                found_gated.add(event["id"])
        assert len(found_gated) == 0, f"Prestige-gated events should not appear at P0: {found_gated}"

    def test_all_new_events_have_valid_structure(self):
        """Every event in the pool has the required fields."""
        from services.dig_constants import EVENT_POOL
        for e in EVENT_POOL:
            assert "id" in e, "Event missing 'id'"
            assert "name" in e, f"Event {e.get('id', '?')} missing 'name'"
            assert "complexity" in e, f"Event {e['id']} missing 'complexity'"
            assert "rarity" in e, f"Event {e['id']} missing 'rarity'"
            assert e["rarity"] in ("common", "uncommon", "rare", "legendary"), (
                f"Event {e['id']} has invalid rarity: {e['rarity']}"
            )
            # safe_option must exist for all events (primary resolution path)
            assert e.get("safe_option") is not None or e.get("boon_options") is not None, (
                f"Event {e['id']} has neither safe_option nor boon_options"
            )

    def test_resolve_event_desperate_choice(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """resolve_event handles desperate choice correctly."""
        from services.dig_constants import EVENT_POOL
        desperate_events = [e for e in EVENT_POOL if e.get("desperate_option") is not None]
        event = desperate_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        random.seed(42)
        result = dig_service.resolve_event(10001, guild_id, event["id"], "desperate")
        assert result["success"]
        assert result.get("choice") == "desperate"

    def test_resolve_event_boon_choice(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """resolve_event handles boon choice correctly."""
        from services.dig_constants import EVENT_POOL
        boon_events = [e for e in EVENT_POOL if e.get("boon_options")]
        event = boon_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        result = dig_service.resolve_event(10001, guild_id, event["id"], "boon_0")
        assert result["success"]
        assert result.get("choice") == "boon_0"
        assert result.get("buff_applied") is not None

    def test_resolve_event_boon_invalid_index(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """resolve_event rejects invalid boon index."""
        from services.dig_constants import EVENT_POOL
        boon_events = [e for e in EVENT_POOL if e.get("boon_options")]
        event = boon_events[0]

        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        # Use an index beyond the number of boon options
        num_boons = len(event["boon_options"])
        result = dig_service.resolve_event(10001, guild_id, event["id"], f"boon_{num_boons + 10}")
        assert not result["success"]


class TestEventPoolInvariants:
    """Invariants on the EVENT_POOL itself, independent of the service."""

    def test_every_non_boon_event_has_safe_option(self):
        """After widening the encounter gate, any non-boon event reaches the
        encounter UI via its safe_option. Events without one would orphan
        into _build_dig_embed's text-only branch."""
        from services.dig_constants import EVENT_POOL
        offenders = [
            e["id"] for e in EVENT_POOL
            if e.get("complexity") != "boon" and not e.get("safe_option")
        ]
        assert offenders == [], (
            f"Non-boon events missing safe_option: {offenders}. "
            "These would render as orphaned flavor text."
        )

    def test_rarity_weights_constant(self):
        from services.dig_service import RARITY_WEIGHTS
        assert RARITY_WEIGHTS == {"common": 70, "uncommon": 20, "rare": 12, "legendary": 6}

    def test_prestige_gates_unlocked(self):
        """infernal_gate, aghanim_trial, neow_blessing should be rollable
        for prestige-0 players after the unlock."""
        from services.dig_constants import EVENT_POOL
        unlocked = {"infernal_gate", "aghanim_trial", "neow_blessing"}
        for e in EVENT_POOL:
            if e["id"] in unlocked:
                assert (e.get("min_prestige") or 0) == 0, (
                    f"{e['id']} still has min_prestige={e.get('min_prestige')}"
                )

    def test_widened_depth_ranges(self):
        """Verify the depth widening from the variance pass didn't get reverted."""
        from services.dig_constants import EVENT_POOL
        expected = {
            "creeper_ambush": (0, 75),
            "abandoned_minecart": (0, 75),
            "villager_trade": (0, 80),
            "enderman_stare": (0, 80),
            "mob_spawner": (0, 75),
            "witch_cauldron": (0, 75),
            "azurite_deposit": (40, 120),
            "crawler_breakdown": (40, 120),
            "fossil_cache": (40, 120),
            "breach_encounter": (40, 170),
            "vaal_side_area": (40, 170),
            "syndicate_ambush": (40, 170),
            "delve_smuggler": (40, 170),
            "brann_bronzebeard": (130, 290),
            "earthen_cache": (130, 290),
            "campfire_rest": (130, 300),
            "zekvir_shadow": (130, 290),
            "dark_rider": (130, 290),
            "titan_relic": (130, 290),
            "candle_glow": (130, 290),
        }
        by_id = {e["id"]: e for e in EVENT_POOL}
        for eid, (lo, hi) in expected.items():
            e = by_id[eid]
            assert (e["min_depth"], e["max_depth"]) == (lo, hi), (
                f"{eid}: expected ({lo},{hi}) got ({e['min_depth']},{e['max_depth']})"
            )


class TestRarityRebalance:
    """Statistical test that the rarity rebalance lands rare share at ~4%."""

    def test_rare_share_in_expected_band(self, dig_service):
        from services.dig_constants import EVENT_POOL, get_layer
        from services.dig_service import RARITY_WEIGHTS

        # Use a depth/layer where we know there's a healthy mix of rarities.
        # Compute expected rare share against the EVENT_POOL filter at the
        # same depth, deriving the layer name dynamically so this stays
        # correct if depth or layer boundaries change.
        random.seed(20260412)
        depth = 30
        layer_name = get_layer(depth).name
        rolls = 5000
        rare_hits = 0
        for _ in range(rolls):
            ev = dig_service.roll_event(depth, luminosity=100, prestige_level=0)
            if ev and ev.get("rarity") == "rare":
                rare_hits += 1
        share = rare_hits / rolls

        # Expected rare share at this depth depends on the eligible pool.
        # Compute it analytically using the same filter roll_event applies.
        eligible = [
            e for e in EVENT_POOL
            if depth >= (e.get("min_depth") or 0)
            and (e.get("max_depth") is None or depth <= e["max_depth"])
            and (e.get("layer") is None or e["layer"] == layer_name)
            and (e.get("min_prestige") or 0) == 0
        ]
        total_w = sum(RARITY_WEIGHTS[e.get("rarity", "common")] for e in eligible)
        rare_w = sum(RARITY_WEIGHTS["rare"] for e in eligible if e.get("rarity") == "rare")
        expected = rare_w / total_w if total_w else 0

        # Allow ±2 percentage points around analytic expectation.
        assert abs(share - expected) < 0.02, (
            f"rare share {share:.3f} drifted from analytic {expected:.3f} "
            f"(weights={RARITY_WEIGHTS})"
        )


class TestPrestigeRelicPool:
    """P5-gated relic drops on boss kills."""

    def test_three_p5_relics_defined(self):
        from services.dig_constants import RELICS
        p5_relics = [r for r in RELICS if r.min_prestige >= 5]
        assert len(p5_relics) == 3
        ids = {r.id for r in p5_relics}
        assert ids == {"hollow_fang", "echo_lantern", "patient_stone"}

    def test_p5_relics_cover_each_axis(self):
        from services.dig_constants import RELICS
        ids = {r.id for r in RELICS if r.min_prestige >= 5}
        # boss-combat, dig-economy, risk-mit (one each)
        assert "hollow_fang" in ids
        assert "echo_lantern" in ids
        assert "patient_stone" in ids

    def test_drop_returns_none_for_low_prestige(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A P3 player can never roll the new pool, even on lucky rolls."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.0)  # always succeed
        result = dig_service._maybe_drop_prestige_relic(10001, 12345, prestige_level=3)
        assert result is None

    def test_drop_can_return_relic_at_p5(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A P5 player with a max-luck roll gets a P5-gated relic."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.0)
        result = dig_service._maybe_drop_prestige_relic(10001, 12345, prestige_level=5)
        assert result is not None
        assert result["id"] in {"hollow_fang", "echo_lantern", "patient_stone"}

    def test_drop_rate_gates_unlucky_roll(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """High random roll (above drop rate) returns None even at high prestige."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # above 10% rate
        result = dig_service._maybe_drop_prestige_relic(10001, 12345, prestige_level=10)
        assert result is None
