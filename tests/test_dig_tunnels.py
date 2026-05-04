"""Tunnel state machine: prestige resets, mutations, ascension, corruption, run scoring,
abandonment, naming, normalization, and hall of fame."""

import json
import random
import time

import pytest

from repositories.dig_repository import DigRepository
from services.dig_constants import (
    ABANDON_REFUND_PCT,
    BOSS_BOUNDARIES,
    FREE_DIG_COOLDOWN_SECONDS,
    MAX_PRESTIGE,
    PINNACLE_DEPTH,
    PRESTIGE_PERKS,
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


class TestPrestige:
    """Tests for prestige system."""

    def _setup_prestige_ready(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Helper to set up a player ready for prestige (all 7 tier bosses
        plus the pinnacle at depth 300 defeated)."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        all_bosses_defeated[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(10001, guild_id, depth=PINNACLE_DEPTH, boss_progress=json.dumps(all_bosses_defeated))

    def test_prestige_resets_depth(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Depth resets to 0."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["depth"] == 0

    def test_prestige_keeps_pickaxe(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Pickaxe carries over."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        dig_repo.update_tunnel(10001, guild_id, pickaxe_tier=1)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["pickaxe_tier"] == 1

    def test_prestige_adds_perk(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Chosen perk is stored."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        perks = json.loads(tunnel["prestige_perks"]) if tunnel["prestige_perks"] else []
        assert "advance_boost" in perks

    def test_prestige_max(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't prestige past MAX_PRESTIGE."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        dig_repo.update_tunnel(10001, guild_id, prestige_level=MAX_PRESTIGE)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert not result["success"]

    def test_prestige_bosses_respawn(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss progress resets."""
        self._setup_prestige_ready(dig_service, dig_repo, player_repository, guild_id, monkeypatch)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        bp = json.loads(tunnel["boss_progress"]) if tunnel["boss_progress"] else {}
        assert all(v == "active" for v in bp.values())

    def test_prestige_grants_jc_and_relic(
        self, dig_service, dig_repo, player_repository, guild_id, monkeypatch,
    ):
        """Prestige hands out 1000 JC + a rare-or-better relic."""
        self._setup_prestige_ready(
            dig_service, dig_repo, player_repository, guild_id, monkeypatch,
        )
        balance_before = player_repository.get_balance(10001, guild_id)
        artifacts_before = dig_repo.get_artifacts(10001, guild_id)

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]

        # JC payout
        balance_after = player_repository.get_balance(10001, guild_id)
        assert balance_after - balance_before == 1000

        # Grant payload exposed
        grant = result["prestige_grant"]
        assert grant["jc"] == 1000
        assert grant["relic"] is not None
        assert grant["relic"]["rarity"] in ("Rare", "Legendary")

        # Relic was actually persisted
        artifacts_after = dig_repo.get_artifacts(10001, guild_id)
        assert len(artifacts_after) == len(artifacts_before) + 1


class TestAbandon:
    """Tests for tunnel abandonment."""

    def test_abandon_refunds_10_percent(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Refund = depth * 0.1."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50)

        balance_before = player_repository.get_balance(10001, guild_id)
        result = dig_service.abandon_tunnel(10001, guild_id)
        assert result["success"]
        balance_after = player_repository.get_balance(10001, guild_id)
        expected_refund = int(50 * ABANDON_REFUND_PCT)
        assert balance_after - balance_before == expected_refund

    def test_abandon_min_depth_10(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Can't abandon below depth 10."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=5)

        result = dig_service.abandon_tunnel(10001, guild_id)
        assert not result["success"]

    def test_abandon_keeps_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Prestige level preserved."""
        _register_player(player_repository, balance=100)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=50, prestige_level=2)

        result = dig_service.abandon_tunnel(10001, guild_id)
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel["prestige_level"] == 2


class TestTunnelNameKey:
    """Verify tunnel_name (not 'name') is used for tunnel display names."""

    def test_help_returns_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """help_tunnel result should contain the actual tunnel name."""
        _register_player(player_repository, discord_id=10001)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        # Create target tunnel
        dig_service.dig(10002, guild_id)
        tunnel = dig_repo.get_tunnel(10002, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")
        assert actual_name  # tunnel has a name

        # Help the target
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        result = dig_service.help_tunnel(10001, 10002, guild_id)
        assert result["success"]
        assert result["target_tunnel"] == actual_name

    def test_sabotage_returns_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """sabotage_tunnel result should contain the actual tunnel name."""
        _register_player(player_repository, discord_id=10001, balance=200)
        _register_player(player_repository, discord_id=10002)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)  # no cave-in
        dig_service.dig(10002, guild_id)
        # Set target depth high enough for sabotage cost
        dig_repo.update_tunnel(10002, guild_id, depth=30)
        tunnel = dig_repo.get_tunnel(10002, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")

        result = dig_service.sabotage_tunnel(10001, 10002, guild_id)
        assert result["success"]
        assert result["target_tunnel"] == actual_name

    def test_get_flex_data_returns_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """get_flex_data should return the actual tunnel name."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")

        result = dig_service.get_flex_data(10001, guild_id)
        assert result["success"]
        assert result["tunnel_name"] == actual_name

    def test_generate_clue_first_letter_uses_tunnel_name(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """_generate_clue should use tunnel_name for the first-letter clue."""
        _register_player(player_repository)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        actual_name = dict(tunnel).get("tunnel_name")
        first_letter = actual_name[0]

        clue = dig_service._generate_clue(10001, guild_id, "first_letter")
        assert first_letter in clue["hint"]


class TestExpandedPrestige:
    """Verify extended prestige and pickaxes."""

    def test_max_prestige_is_10(self):
        from services.dig_constants import MAX_PRESTIGE
        assert MAX_PRESTIGE == 10

    def test_pickaxe_tiers_define_full_ladder(self):
        from services.dig_constants import _PICKAXE_TIERS_DEF
        # 8 tiers: Wooden..Diamond, Obsidian, Stormrend (P2 unlock), Frostforged, Void-Touched
        assert len(_PICKAXE_TIERS_DEF) == 8
        assert _PICKAXE_TIERS_DEF[-1].name == "Void-Touched"
        assert _PICKAXE_TIERS_DEF[5].name == "Stormrend"

    def test_nine_prestige_perks(self):
        assert len(PRESTIGE_PERKS) == 12
        assert "deep_sight" in PRESTIGE_PERKS
        assert "the_endless" in PRESTIGE_PERKS
        assert "patient_step" in PRESTIGE_PERKS
        assert "steady_hands" in PRESTIGE_PERKS
        assert "reading_the_stone" in PRESTIGE_PERKS

    def test_crowns_for_all_levels(self):
        from services.dig_constants import MAX_PRESTIGE, PRESTIGE_CROWNS
        for i in range(MAX_PRESTIGE + 1):
            assert i in PRESTIGE_CROWNS, f"Missing crown for prestige {i}"


class TestTunnelNormalization:
    """Tests for integer type coercion in tunnel data."""

    def test_normalize_tunnel_casts_string_ints(self, dig_repo):
        """_normalize_tunnel converts string values to int for known columns."""
        raw = {
            "depth": "49",
            "luminosity": "100",
            "last_dig_at": "1000000",
            "boss_attempts": "3",
            "tunnel_name": "Test Tunnel",
            "boss_progress": '{"50": "active"}',
        }
        normalized = DigRepository._normalize_tunnel(raw)
        assert normalized["depth"] == 49
        assert isinstance(normalized["depth"], int)
        assert normalized["luminosity"] == 100
        assert isinstance(normalized["luminosity"], int)
        assert normalized["last_dig_at"] == 1000000
        assert normalized["boss_attempts"] == 3
        # Non-int columns left alone
        assert normalized["tunnel_name"] == "Test Tunnel"
        assert normalized["boss_progress"] == '{"50": "active"}'

    def test_normalize_tunnel_handles_none_values(self, dig_repo):
        """_normalize_tunnel leaves None values as None."""
        raw = {"depth": 10, "last_dig_at": None, "luminosity": None}
        normalized = DigRepository._normalize_tunnel(raw)
        assert normalized["depth"] == 10
        assert normalized["last_dig_at"] is None
        assert normalized["luminosity"] is None

    def test_get_tunnel_returns_int_types(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """get_tunnel returns integer types for numeric columns."""
        _register_player(player_repository, balance=200)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)

        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert isinstance(tunnel["depth"], int)
        assert isinstance(tunnel["luminosity"], int)
        assert isinstance(tunnel["last_dig_at"], int)


class TestAscensionSystem:
    """Test ascension modifier mechanics."""

    def test_get_ascension_effects_level_0(self, dig_service):
        """No effects at prestige 0."""
        effects = dig_service._get_ascension_effects(0)
        assert effects == {}

    def test_get_ascension_effects_level_1(self, dig_service):
        """Level 1 returns jc_multiplier (no advance penalty)."""
        effects = dig_service._get_ascension_effects(1)
        assert "advance_penalty" not in effects
        assert "jc_multiplier" in effects
        assert effects["jc_multiplier"] == 0.25

    def test_ascension_effects_cumulative(self, dig_service):
        """Multiple levels stack their effects."""
        effects = dig_service._get_ascension_effects(3)
        # Level 1 jc_multiplier=0.25 (no advance_penalty)
        assert "advance_penalty" not in effects
        assert effects["jc_multiplier"] == 0.25
        # Level 2 cave_in_bonus=0.03
        assert effects["cave_in_bonus"] == 0.03
        # Level 2 event_chance_multiplier=0.20
        assert effects["event_chance_multiplier"] == 0.20
        # Level 3 luminosity_drain_multiplier=0.25
        assert effects["luminosity_drain_multiplier"] == 0.25
        # Level 3 rare_event_multiplier=0.50
        assert effects["rare_event_multiplier"] == 0.50

    def test_boss_phase2_at_prestige_2(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss fight at P2+ returns phase2_incoming on first win.

        Phase gate was lowered from P4 to P2 in the boss revamp; phase 2
        unlocks at first prestige cycle so most active players can experience
        the multi-phase fights.
        """
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24, prestige_level=2)

        # Cautious @ depth 25, P2 with wager=10: player_hit 0.60 − 0.01 − 0.02
        # = 0.57 (no free-fight mod). boss_hit=0.30 + tank archetype offset
        # depending on rolled boss; we pin grothak (bruiser) below.
        progress = json.dumps({"25": {"boss_id": "grothak", "status": "active"}})
        dig_repo.update_tunnel(10001, guild_id, boss_progress=progress)
        # Player hits, boss misses — alternating roll sequence overrides the
        # blanket 0.99 set above. Sized for the post-revamp boss HP at P2.
        rolls = iter([0.0, 0.99] * 100)
        monkeypatch.setattr(random, "random", lambda: next(rolls))
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won") is True
        # At P2+, boss should enter phase 2 on first victory
        assert result.get("phase2_incoming") is True
        assert result.get("phase") == 1

        # Boss progress should be "phase1_defeated"
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        bp = json.loads(tunnel["boss_progress"])
        # boss_progress entry is now a dict; support either shape post-migration.
        entry = bp["25"]
        status = entry.get("status") if isinstance(entry, dict) else entry
        assert status == "phase1_defeated"

    def test_boss_no_phase2_below_prestige_2(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Boss fight below P2 goes straight to defeated.

        With the new gate, phase 2 only unlocks at P2+. P0 and P1 players
        see a single-phase fight and the boss goes directly to ``defeated``.
        """
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        dig_repo.update_tunnel(10001, guild_id, depth=24, prestige_level=1)
        progress = json.dumps({"25": {"boss_id": "grothak", "status": "active"}})
        dig_repo.update_tunnel(10001, guild_id, boss_progress=progress)

        # Player hits, boss misses — alternating roll sequence so the test
        # deterministically wins despite the post-revamp HP/hit scaling.
        rolls = iter([0.0, 0.99] * 100)
        monkeypatch.setattr(random, "random", lambda: next(rolls))
        result = dig_service.fight_boss(10001, guild_id, "cautious", wager=10)
        assert result["success"]
        assert result.get("won") is True
        # No phase 2 at P1
        assert result.get("phase2_incoming") is not True

        # Boss should go straight to defeated
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        bp = json.loads(tunnel["boss_progress"])
        entry = bp["25"]
        status = entry.get("status") if isinstance(entry, dict) else entry
        assert status == "defeated"


class TestCorruptionSystem:
    """Test corruption roll mechanics."""

    def test_no_corruption_below_p6(self, dig_service):
        """_roll_corruption returns None at prestige < 6."""
        for level in range(6):
            result = dig_service._roll_corruption(level)
            assert result is None, f"Expected None at prestige {level}"

    def test_corruption_at_p6(self, dig_service):
        """_roll_corruption returns an effect at prestige 6+."""
        random.seed(42)
        result = dig_service._roll_corruption(6)
        assert result is not None
        assert "id" in result
        assert "description" in result
        assert "effects" in result
        assert isinstance(result["effects"], dict)

    def test_corruption_effect_has_valid_fields(self, dig_service):
        """Corruption effect dict has all expected fields."""
        random.seed(0)
        # Run multiple times to cover both bad and weird paths
        found_any = False
        for seed in range(50):
            random.seed(seed)
            result = dig_service._roll_corruption(8)
            assert result is not None
            assert "id" in result
            assert "weird" in result
            assert isinstance(result["weird"], bool)
            found_any = True
        assert found_any

    def test_corruption_weird_ratio(self, dig_service):
        """Corruption rolls are ~80% bad / ~20% weird over many trials."""
        random.seed(12345)
        weird_count = 0
        total = 500
        for _ in range(total):
            result = dig_service._roll_corruption(6)
            if result["weird"]:
                weird_count += 1
        # Should be roughly 20% weird (allow 10%-35% tolerance for randomness)
        assert 50 <= weird_count <= 175, f"Weird ratio {weird_count}/{total} outside expected range"


class TestMutationSystem:
    """Test mutation mechanics."""

    def test_roll_mutations_returns_forced_and_choices(self, dig_service):
        """_roll_mutations_for_prestige returns (forced, choices_list)."""
        random.seed(42)
        forced, choices = dig_service._roll_mutations_for_prestige()
        # forced is a single dict
        assert isinstance(forced, dict)
        assert "id" in forced
        assert "name" in forced
        assert "description" in forced
        assert "positive" in forced
        # choices is a list of dicts
        assert isinstance(choices, list)
        assert len(choices) == 3
        for c in choices:
            assert "id" in c
            assert "name" in c
        # forced should not be in choices
        choice_ids = {c["id"] for c in choices}
        assert forced["id"] not in choice_ids

    def test_apply_mutation_effects(self, dig_service):
        """_apply_mutation_effects combines effect dicts."""
        mutations = [
            {"id": "cave_in_loot"},
            {"id": "brittle_walls"},
        ]
        combined = dig_service._apply_mutation_effects(mutations)
        # cave_in_loot has cave_in_loot_chance=0.30
        assert combined.get("cave_in_loot_chance") == 0.30
        # brittle_walls has cave_in_loss_bonus=2
        assert combined.get("cave_in_loss_bonus") == 2

    def test_apply_mutation_effects_stacks_numeric(self, dig_service):
        """Numeric mutation effects from multiple mutations stack additively."""
        # Two mutations with the same numeric key should add
        mutations = [
            {"id": "event_magnet"},     # event_chance_bonus=0.30
            {"id": "treasure_sense"},   # artifact_chance_bonus=0.25
        ]
        combined = dig_service._apply_mutation_effects(mutations)
        assert combined.get("event_chance_bonus") == 0.30
        assert combined.get("artifact_chance_bonus") == 0.25

    def test_get_mutations_empty(self, dig_service):
        """_get_mutations returns empty list for tunnel with no mutations."""
        tunnel = {"mutations": None}
        assert dig_service._get_mutations(tunnel) == []
        tunnel2 = {"mutations": ""}
        assert dig_service._get_mutations(tunnel2) == []

    def test_get_mutations_parses_json(self, dig_service):
        """_get_mutations parses stored JSON correctly."""
        data = [{"id": "cave_in_loot", "name": "Lucky Rubble"}]
        tunnel = {"mutations": json.dumps(data)}
        result = dig_service._get_mutations(tunnel)
        assert len(result) == 1
        assert result[0]["id"] == "cave_in_loot"

    def test_mutations_stored_in_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Prestige at P8+ stores mutations in tunnel."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        all_bosses_defeated[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, guild_id, depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(all_bosses_defeated),
            prestige_level=7,  # After prestige will become P8
        )

        random.seed(42)
        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        assert result["prestige_level"] == 8

        # Mutations should be stored
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        mutations_raw = tunnel.get("mutations")
        assert mutations_raw is not None
        mutations = json.loads(mutations_raw)
        assert len(mutations) >= 1  # At least the forced mutation
        # Result should contain mutation info
        assert result.get("mutations") is not None


class TestRunScoring:
    """Test run score calculation."""

    def test_calculate_run_score_basic(self, dig_service):
        """Score based on depth + bosses + JC + artifacts + events."""
        tunnel = {
            "depth": 100,
            "boss_progress": json.dumps({"25": "defeated", "50": "defeated", "75": "active", "100": "active"}),
            "current_run_jc": 40,
            "current_run_artifacts": 2,
            "current_run_events": 5,
            "prestige_level": 0,
        }
        score = dig_service._calculate_run_score(tunnel)
        # base = depth*1 + bosses_defeated*50 + int(jc*0.5) + artifacts*25 + events*10
        # = 100 + 2*50 + int(40*0.5) + 2*25 + 5*10
        # = 100 + 100 + 20 + 50 + 50 = 320
        # multiplier = 1 + 0*0.1 = 1.0
        expected = int(320 * 1.0)
        assert score == expected

    def test_score_multiplier_at_higher_prestige(self, dig_service):
        """Higher prestige levels multiply the score."""
        tunnel_base = {
            "depth": 50,
            "boss_progress": json.dumps({"25": "defeated", "50": "active"}),
            "current_run_jc": 20,
            "current_run_artifacts": 1,
            "current_run_events": 3,
            "prestige_level": 0,
        }
        score_p0 = dig_service._calculate_run_score(tunnel_base)

        tunnel_p5 = dict(tunnel_base)
        tunnel_p5["prestige_level"] = 5
        score_p5 = dig_service._calculate_run_score(tunnel_p5)
        # multiplier at P5 = 1 + 5*0.1 = 1.5
        assert score_p5 > score_p0
        assert score_p5 == int(score_p0 * 1.5)

    def test_score_multiplier_p10_includes_ascension(self, dig_service):
        """P10 'The Endless' adds score_multiplier=2.0 on top of base multiplier."""
        tunnel = {
            "depth": 100,
            "boss_progress": json.dumps({}),
            "current_run_jc": 0,
            "current_run_artifacts": 0,
            "current_run_events": 0,
            "prestige_level": 10,
        }
        score = dig_service._calculate_run_score(tunnel)
        # base = 100
        # base multiplier = 1 + 10*0.1 = 2.0
        # ascension score_multiplier at P10 = 2.0
        # total multiplier = 2.0 + 2.0 = 4.0
        expected = int(100 * 4.0)
        assert score == expected

    def test_prestige_stores_run_score(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Prestige stores best_run_score and resets counters."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        all_bosses_defeated[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(all_bosses_defeated),
            current_run_jc=50,
            current_run_artifacts=3,
            current_run_events=10,
        )

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]
        assert result["run_score"] > 0
        assert result["best_run_score"] > 0

        # Counters should be reset
        tunnel = dig_repo.get_tunnel(10001, guild_id)
        assert tunnel.get("current_run_jc") == 0 or tunnel["current_run_jc"] == 0
        assert tunnel.get("current_run_artifacts") == 0 or tunnel["current_run_artifacts"] == 0
        assert tunnel.get("current_run_events") == 0 or tunnel["current_run_events"] == 0


class TestHallOfFame:
    """Test hall of fame leaderboard."""

    def test_hall_of_fame_empty_guild(self, dig_service, guild_id):
        """Hall of fame returns empty list for guild with no scores."""
        result = dig_service.get_hall_of_fame(guild_id)
        assert result["success"]
        assert result["entries"] == []

    def test_hall_of_fame_after_prestige(self, dig_service, dig_repo, player_repository, guild_id, monkeypatch):
        """Hall of fame shows player after prestige with run score."""
        _register_player(player_repository, balance=500)
        monkeypatch.setattr(time, "time", lambda: 1_000_000)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, guild_id)
        all_bosses_defeated = {str(b): "defeated" for b in BOSS_BOUNDARIES}
        all_bosses_defeated[str(PINNACLE_DEPTH)] = "defeated"
        dig_repo.update_tunnel(
            10001, guild_id,
            depth=PINNACLE_DEPTH,
            boss_progress=json.dumps(all_bosses_defeated),
            current_run_jc=30,
            current_run_artifacts=2,
            current_run_events=5,
        )

        result = dig_service.prestige(10001, guild_id, "advance_boost")
        assert result["success"]

        hof = dig_service.get_hall_of_fame(guild_id)
        assert hof["success"]
        assert len(hof["entries"]) == 1
        assert hof["entries"][0]["discord_id"] == 10001
        assert hof["entries"][0]["best_run_score"] > 0
