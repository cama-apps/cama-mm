"""Tests for the mana effects system.

Covers:
- ManaEffects data class: default values and per-color factory method
- ManaEffectsService: effect resolution, siphon, blue tax/cashback, green cap, tithe
- Integration: verifying numeric effect values flow correctly
"""

from __future__ import annotations

import pytest
from unittest.mock import MagicMock, patch

from domain.models.mana_effects import ManaEffects
from repositories.mana_repository import ManaRepository
from repositories.player_repository import PlayerRepository
from services.mana_effects_service import ManaEffectsService
from services.mana_service import ManaService, get_today_pst
from tests.conftest import TEST_GUILD_ID

GID = TEST_GUILD_ID


# =============================================================================
# ManaEffects data class tests
# =============================================================================


class TestManaEffects:
    """Tests for ManaEffects data class."""

    def test_default_effects(self):
        """Default ManaEffects has no active effects."""
        e = ManaEffects()
        assert e.color is None
        assert e.land is None
        assert e.red_10x_leverage is False
        assert e.blue_gamba_scrying is False
        assert e.green_steady_bonus == 0
        assert e.green_gain_cap is None
        assert e.plains_guardian_aura is False
        assert e.plains_max_wheel_win is None
        assert e.plains_tip_fee_rate is None
        assert e.swamp_siphon is False
        assert e.swamp_self_tax == 0
        assert e.swamp_bankruptcy_games == 5

    def test_red_effects(self):
        """Red/Mountain has high-risk gambling modifiers."""
        e = ManaEffects.for_color("Red", "Mountain")
        assert e.color == "Red"
        assert e.land == "Mountain"
        assert e.red_10x_leverage is True
        assert e.red_bomb_pot_ante == 30
        assert e.red_roll_cost == 2
        assert e.red_roll_jackpot == 40

    def test_blue_effects(self):
        """Blue/Island has scrying, reduction, cashback, and tax."""
        e = ManaEffects.for_color("Blue", "Island")
        assert e.color == "Blue"
        assert e.land == "Island"
        assert e.blue_gamba_scrying is True
        assert e.blue_gamba_reduction == 0.25
        assert e.blue_cashback_rate == 0.05
        assert e.blue_tax_rate == 0.05

    def test_green_effects(self):
        """Green/Forest has steady bonus, gain cap, and compressed variance."""
        e = ManaEffects.for_color("Green", "Forest")
        assert e.color == "Green"
        assert e.land == "Forest"
        assert e.green_steady_bonus == 1
        assert e.green_gain_cap == 50
        assert e.green_bankrupt_penalty == -50
        assert e.green_max_wheel_win == 60

    def test_white_effects(self):
        """White/Plains has guardian aura, max win cap, free tips, and tithe."""
        e = ManaEffects.for_color("White", "Plains")
        assert e.color == "White"
        assert e.land == "Plains"
        assert e.plains_guardian_aura is True
        assert e.plains_max_wheel_win == 50
        assert e.plains_tip_fee_rate == 0.0
        assert e.plains_tithe_rate == 0.05

    def test_black_effects(self):
        """Black/Swamp has siphon, self-tax, and reduced bankruptcy games."""
        e = ManaEffects.for_color("Black", "Swamp")
        assert e.color == "Black"
        assert e.land == "Swamp"
        assert e.swamp_siphon is True
        assert e.swamp_self_tax == 2
        assert e.swamp_bankruptcy_games == 3

    def test_none_color(self):
        """for_color(None, None) returns defaults with no effects."""
        e = ManaEffects.for_color(None, None)
        assert e.color is None
        assert e.land is None
        assert e.red_10x_leverage is False
        assert e.blue_gamba_scrying is False
        assert e.green_steady_bonus == 0
        assert e.plains_guardian_aura is False
        assert e.swamp_siphon is False

    def test_red_does_not_activate_other_colors(self):
        """Red effects should not activate Blue, Green, White, or Black effects."""
        e = ManaEffects.for_color("Red", "Mountain")
        assert e.blue_gamba_scrying is False
        assert e.green_steady_bonus == 0
        assert e.plains_guardian_aura is False
        assert e.swamp_siphon is False

    def test_default_non_color_fields_unchanged(self):
        """Non-color defaults should stay for a colored instance."""
        e = ManaEffects.for_color("Blue", "Island")
        # Red defaults untouched
        assert e.red_10x_leverage is False
        assert e.red_roll_cost == 1
        # Green defaults untouched
        assert e.green_gain_cap is None
        # Black defaults untouched
        assert e.swamp_bankruptcy_games == 5


# =============================================================================
# Fixtures for ManaEffectsService tests
# =============================================================================


def _make_mana_service(mana_repo, player_repo):
    """Create a ManaService with mocked non-repo dependencies."""
    gambling_stats = MagicMock()
    gambling_stats.calculate_degen_score.return_value = MagicMock(total=0)
    gambling_stats.bet_repo = MagicMock()
    gambling_stats.bet_repo.get_player_bet_history.return_value = []

    bankruptcy_service = MagicMock()
    bankruptcy_service.get_state.return_value = MagicMock(
        penalty_games_remaining=0, last_bankruptcy_at=None
    )

    tip_repo = MagicMock()
    tip_repo.get_user_tip_stats.return_value = {"total_sent": 0, "tips_sent_count": 0}

    svc = ManaService(
        mana_repo=mana_repo,
        player_repo=player_repo,
        gambling_stats_service=gambling_stats,
        bankruptcy_service=bankruptcy_service,
        tip_repo=tip_repo,
    )
    return svc


def _register_player(player_repo, discord_id, guild_id=GID, balance=100):
    """Helper to register a player with a given balance."""
    player_repo.add(
        discord_id=discord_id,
        discord_username=f"TestPlayer{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    if balance != 0:
        player_repo.update_balance(discord_id, guild_id, balance)


# =============================================================================
# ManaEffectsService tests
# =============================================================================


class TestManaEffectsService:
    """Tests for ManaEffectsService."""

    @pytest.fixture
    def service(self, repo_db_path):
        mana_repo = ManaRepository(repo_db_path)
        player_repo = PlayerRepository(repo_db_path)
        mana_service = _make_mana_service(mana_repo, player_repo)

        effects_service = ManaEffectsService(
            mana_service=mana_service,
            player_repo=player_repo,
            mana_repo=mana_repo,
        )

        return {
            "effects_service": effects_service,
            "mana_service": mana_service,
            "mana_repo": mana_repo,
            "player_repo": player_repo,
        }

    def test_get_effects_no_mana(self, service):
        """Player without mana assignment gets default effects."""
        _register_player(service["player_repo"], 99999)
        effects = service["effects_service"].get_effects(99999, GID)
        assert effects.color is None
        assert effects.red_10x_leverage is False
        assert effects.blue_gamba_scrying is False

    def test_get_effects_with_mana(self, service):
        """Player with today's mana gets correct effects."""
        _register_player(service["player_repo"], 10001)
        today = get_today_pst()
        service["mana_repo"].set_mana(10001, GID, "Mountain", today)

        effects = service["effects_service"].get_effects(10001, GID)
        assert effects.color == "Red"
        assert effects.red_10x_leverage is True
        assert effects.red_roll_cost == 2
        assert effects.red_roll_jackpot == 40
        assert effects.red_bomb_pot_ante == 30

    def test_get_effects_stale_mana(self, service):
        """Player with yesterday's mana gets default (no) effects."""
        _register_player(service["player_repo"], 10002)
        service["mana_repo"].set_mana(10002, GID, "Mountain", "2020-01-01")

        effects = service["effects_service"].get_effects(10002, GID)
        assert effects.color is None
        assert effects.red_10x_leverage is False

    def test_get_effects_blue(self, service):
        """Player with Blue mana gets Blue effects."""
        _register_player(service["player_repo"], 10003)
        today = get_today_pst()
        service["mana_repo"].set_mana(10003, GID, "Island", today)

        effects = service["effects_service"].get_effects(10003, GID)
        assert effects.color == "Blue"
        assert effects.blue_gamba_scrying is True
        assert effects.blue_tax_rate == 0.05

    def test_get_effects_green(self, service):
        """Player with Green mana gets Green effects."""
        _register_player(service["player_repo"], 10004)
        today = get_today_pst()
        service["mana_repo"].set_mana(10004, GID, "Forest", today)

        effects = service["effects_service"].get_effects(10004, GID)
        assert effects.color == "Green"
        assert effects.green_steady_bonus == 1
        assert effects.green_gain_cap == 50

    def test_get_effects_white(self, service):
        """Player with White mana gets White/Plains effects."""
        _register_player(service["player_repo"], 10005)
        today = get_today_pst()
        service["mana_repo"].set_mana(10005, GID, "Plains", today)

        effects = service["effects_service"].get_effects(10005, GID)
        assert effects.color == "White"
        assert effects.plains_guardian_aura is True

    def test_get_effects_black(self, service):
        """Player with Black mana gets Black/Swamp effects."""
        _register_player(service["player_repo"], 10006)
        today = get_today_pst()
        service["mana_repo"].set_mana(10006, GID, "Swamp", today)

        effects = service["effects_service"].get_effects(10006, GID)
        assert effects.color == "Black"
        assert effects.swamp_siphon is True
        assert effects.swamp_bankruptcy_games == 3

    # -------------------------------------------------------------------------
    # execute_siphon
    # -------------------------------------------------------------------------

    def test_execute_siphon(self, service):
        """Swamp siphon steals 1-3 JC from a random positive-balance player."""
        _register_player(service["player_repo"], 20001, balance=50)
        _register_player(service["player_repo"], 20002, balance=100)

        result = service["effects_service"].execute_siphon(20001, GID)
        assert result is not None
        assert result["victim_id"] == 20002
        assert 1 <= result["amount"] <= 3
        assert isinstance(result["anonymous"], bool)

    def test_execute_siphon_transfers_balance(self, service):
        """Siphon actually moves JC between players."""
        _register_player(service["player_repo"], 21001, balance=50)
        _register_player(service["player_repo"], 21002, balance=100)

        result = service["effects_service"].execute_siphon(21001, GID)
        assert result is not None
        amount = result["amount"]

        thief_bal = service["player_repo"].get_balance(21001, GID)
        victim_bal = service["player_repo"].get_balance(21002, GID)
        assert thief_bal == 50 + amount
        assert victim_bal == 100 - amount

    def test_execute_siphon_no_targets(self, service):
        """Siphon returns None when no eligible targets exist."""
        _register_player(service["player_repo"], 30001, balance=50)
        # No other players registered
        result = service["effects_service"].execute_siphon(30001, GID)
        assert result is None

    def test_execute_siphon_skips_zero_balance(self, service):
        """Siphon skips players with zero balance."""
        _register_player(service["player_repo"], 31001, balance=50)
        # Register player then set balance to exactly 0
        _register_player(service["player_repo"], 31002, balance=1)
        service["player_repo"].update_balance(31002, GID, 0)
        # Only target has zero balance
        result = service["effects_service"].execute_siphon(31001, GID)
        assert result is None

    # -------------------------------------------------------------------------
    # apply_blue_tax
    # -------------------------------------------------------------------------

    def test_apply_blue_tax(self, service):
        """Blue tax deducts 5% of gains."""
        _register_player(service["player_repo"], 40001, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40001, GID, "Island", today)

        tax = service["effects_service"].apply_blue_tax(40001, GID, 100)
        assert tax == 5  # 5% of 100
        bal = service["player_repo"].get_balance(40001, GID)
        assert bal == 95  # 100 - 5

    def test_apply_blue_tax_minimum_one(self, service):
        """Blue tax is at least 1 JC for any positive gain."""
        _register_player(service["player_repo"], 40003, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40003, GID, "Island", today)

        tax = service["effects_service"].apply_blue_tax(40003, GID, 5)
        assert tax == 1  # max(1, int(5 * 0.05)) = max(1, 0) = 1

    def test_apply_blue_tax_zero_gain(self, service):
        """Blue tax returns 0 for zero gain."""
        _register_player(service["player_repo"], 40004, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40004, GID, "Island", today)

        tax = service["effects_service"].apply_blue_tax(40004, GID, 0)
        assert tax == 0
        bal = service["player_repo"].get_balance(40004, GID)
        assert bal == 100  # unchanged

    def test_apply_blue_tax_no_blue_mana(self, service):
        """Non-blue player gets no tax."""
        _register_player(service["player_repo"], 40005, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40005, GID, "Mountain", today)

        tax = service["effects_service"].apply_blue_tax(40005, GID, 100)
        assert tax == 0
        bal = service["player_repo"].get_balance(40005, GID)
        assert bal == 100

    # -------------------------------------------------------------------------
    # apply_blue_cashback
    # -------------------------------------------------------------------------

    def test_apply_blue_cashback(self, service):
        """Blue cashback returns 5% of losses."""
        _register_player(service["player_repo"], 40002, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40002, GID, "Island", today)

        cashback = service["effects_service"].apply_blue_cashback(40002, GID, 20)
        assert cashback == 1  # max(1, int(20 * 0.05)) = 1
        bal = service["player_repo"].get_balance(40002, GID)
        assert bal == 101  # 100 + 1

    def test_apply_blue_cashback_larger_loss(self, service):
        """Blue cashback on larger loss."""
        _register_player(service["player_repo"], 40006, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40006, GID, "Island", today)

        cashback = service["effects_service"].apply_blue_cashback(40006, GID, 100)
        assert cashback == 5  # 5% of 100
        bal = service["player_repo"].get_balance(40006, GID)
        assert bal == 105

    def test_apply_blue_cashback_zero_loss(self, service):
        """Blue cashback returns 0 for zero loss."""
        _register_player(service["player_repo"], 40007, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40007, GID, "Island", today)

        cashback = service["effects_service"].apply_blue_cashback(40007, GID, 0)
        assert cashback == 0

    def test_apply_blue_cashback_no_blue_mana(self, service):
        """Non-blue player gets no cashback."""
        _register_player(service["player_repo"], 40008, balance=100)
        today = get_today_pst()
        service["mana_repo"].set_mana(40008, GID, "Forest", today)

        cashback = service["effects_service"].apply_blue_cashback(40008, GID, 100)
        assert cashback == 0

    # -------------------------------------------------------------------------
    # apply_green_cap
    # -------------------------------------------------------------------------

    def test_apply_green_cap(self, service):
        """Green gain cap limits gains to 50."""
        effects = ManaEffects.for_color("Green", "Forest")
        capped = service["effects_service"].apply_green_cap(effects, 100)
        assert capped == 50

    def test_apply_green_cap_below_cap(self, service):
        """Gains below cap are unchanged."""
        effects = ManaEffects.for_color("Green", "Forest")
        capped = service["effects_service"].apply_green_cap(effects, 30)
        assert capped == 30

    def test_apply_green_cap_exactly_at_cap(self, service):
        """Gains exactly at cap are unchanged."""
        effects = ManaEffects.for_color("Green", "Forest")
        capped = service["effects_service"].apply_green_cap(effects, 50)
        assert capped == 50

    def test_apply_green_cap_no_green(self, service):
        """Non-green effects have no cap (gain_cap is None)."""
        effects = ManaEffects.for_color("Red", "Mountain")
        capped = service["effects_service"].apply_green_cap(effects, 200)
        assert capped == 200

    def test_apply_green_cap_default_no_cap(self, service):
        """Default effects have no gain cap."""
        effects = ManaEffects()
        capped = service["effects_service"].apply_green_cap(effects, 999)
        assert capped == 999

    # -------------------------------------------------------------------------
    # apply_plains_tithe
    # -------------------------------------------------------------------------

    def test_apply_plains_tithe(self, service):
        """Plains tithe deducts 5% of gains."""
        _register_player(service["player_repo"], 50001, balance=200)
        today = get_today_pst()
        service["mana_repo"].set_mana(50001, GID, "Plains", today)

        tithe = service["effects_service"].apply_plains_tithe(50001, GID, 100)
        assert tithe == 5  # 5% of 100
        bal = service["player_repo"].get_balance(50001, GID)
        assert bal == 195  # 200 - 5

    def test_apply_plains_tithe_zero_gain(self, service):
        """Plains tithe returns 0 for zero gain."""
        _register_player(service["player_repo"], 50002, balance=200)
        today = get_today_pst()
        service["mana_repo"].set_mana(50002, GID, "Plains", today)

        tithe = service["effects_service"].apply_plains_tithe(50002, GID, 0)
        assert tithe == 0
        bal = service["player_repo"].get_balance(50002, GID)
        assert bal == 200

    def test_apply_plains_tithe_no_plains(self, service):
        """Non-plains player has no tithe."""
        _register_player(service["player_repo"], 50003, balance=200)
        today = get_today_pst()
        service["mana_repo"].set_mana(50003, GID, "Mountain", today)

        tithe = service["effects_service"].apply_plains_tithe(50003, GID, 100)
        assert tithe == 0
        bal = service["player_repo"].get_balance(50003, GID)
        assert bal == 200


# =============================================================================
# Integration tests - verifying numeric effect values
# =============================================================================


class TestManaEffectsIntegration:
    """Integration tests verifying effect values flow correctly."""

    def test_red_roll_cost_and_jackpot(self):
        """Red effects have roll cost 2 and jackpot 40."""
        effects = ManaEffects.for_color("Red", "Mountain")
        assert effects.red_roll_cost == 2
        assert effects.red_roll_jackpot == 40

    def test_green_gain_cap_applied(self):
        """Green cap limits gain to 50 via apply_green_cap."""
        effects = ManaEffects.for_color("Green", "Forest")
        # Simulate the cap logic inline (same as service method)
        gain = 120
        if effects.green_gain_cap is not None and gain > effects.green_gain_cap:
            gain = effects.green_gain_cap
        assert gain == 50

    def test_swamp_bankruptcy_games(self):
        """Swamp gives 3 penalty games (reduced from default 5)."""
        effects = ManaEffects.for_color("Black", "Swamp")
        assert effects.swamp_bankruptcy_games == 3

        default = ManaEffects()
        assert default.swamp_bankruptcy_games == 5
        assert effects.swamp_bankruptcy_games < default.swamp_bankruptcy_games

    def test_red_bomb_pot_ante_elevated(self):
        """Red bomb pot ante is 30 (vs default 10)."""
        effects = ManaEffects.for_color("Red", "Mountain")
        default = ManaEffects()
        assert effects.red_bomb_pot_ante == 30
        assert default.red_bomb_pot_ante == 10
        assert effects.red_bomb_pot_ante > default.red_bomb_pot_ante

    def test_green_compressed_variance(self):
        """Green bankrupt penalty is -50 (vs default -100)."""
        effects = ManaEffects.for_color("Green", "Forest")
        default = ManaEffects()
        assert effects.green_bankrupt_penalty == -50
        assert default.green_bankrupt_penalty == -100

    def test_green_max_wheel_win_reduced(self):
        """Green max wheel win is 60 (vs default 100)."""
        effects = ManaEffects.for_color("Green", "Forest")
        default = ManaEffects()
        assert effects.green_max_wheel_win == 60
        assert default.green_max_wheel_win == 100

    def test_all_colors_set_identity_fields(self):
        """All for_color calls set color and land fields correctly."""
        pairs = [
            ("Red", "Mountain"),
            ("Blue", "Island"),
            ("Green", "Forest"),
            ("White", "Plains"),
            ("Black", "Swamp"),
        ]
        for color, land in pairs:
            e = ManaEffects.for_color(color, land)
            assert e.color == color, f"Expected color={color}, got {e.color}"
            assert e.land == land, f"Expected land={land}, got {e.land}"
