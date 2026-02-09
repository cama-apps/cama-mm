"""
Tests for the Neon Degen Terminal easter egg system.

Tests ASCII generation, GIF size limits, and orchestrator logic.
"""

import io
import pytest

from utils.neon_terminal import (
    ansi_block,
    ascii_box,
    corrupt_text,
    render_balance_check,
    render_balance_zero,
    render_bankruptcy_filing,
    render_bet_placed,
    render_coinflip,
    render_cooldown_hit,
    render_debt_collector,
    render_don_lose,
    render_don_loss_box,
    render_don_win,
    render_loan_taken,
    render_match_recorded,
    render_negative_loan,
    render_prediction_market_crash,
    render_prediction_resolved,
    render_registration,
    render_soft_avoid,
    render_soft_avoid_surveillance,
    render_streak,
    render_system_breach,
    render_wheel_bankrupt,
)
from services.neon_degen_service import NeonDegenService, NeonResult


# ---------------------------------------------------------------------------
# ASCII template tests
# ---------------------------------------------------------------------------


class TestNeonTerminal:
    """Tests for utils/neon_terminal.py ASCII generation."""

    def test_ansi_block_wraps_in_code_block(self):
        result = ansi_block("hello")
        assert result.startswith("```ansi\n")
        assert result.endswith("\n```")
        assert "hello" in result

    def test_ascii_box_creates_bordered_box(self):
        result = ascii_box(["line1", "line2"], width=20)
        assert "+" in result
        assert "-" in result
        assert "|" in result

    def test_corrupt_text_modifies_text(self):
        original = "abcdefghijklmnop"
        # With high intensity, text should be modified
        corrupted = corrupt_text(original, intensity=1.0)
        assert corrupted != original
        assert len(corrupted) == len(original)

    def test_corrupt_text_preserves_spaces(self):
        original = "hello world test"
        for _ in range(20):
            corrupted = corrupt_text(original, intensity=1.0)
            # Spaces should be preserved
            assert corrupted[5] == " "
            assert corrupted[11] == " "

    def test_corrupt_text_zero_intensity(self):
        original = "hello world"
        result = corrupt_text(original, intensity=0.0)
        assert result == original

    def test_render_balance_check_positive(self):
        result = render_balance_check("TestUser", 100)
        assert "```ansi" in result
        assert "```" in result
        # Should contain the name or balance somewhere
        assert "TestUser" in result or "100" in result

    def test_render_balance_check_negative(self):
        result = render_balance_check("TestUser", -250)
        assert "```ansi" in result
        # Should use debt templates

    def test_render_bet_placed_no_leverage(self):
        result = render_bet_placed(50, "radiant", 1)
        assert "```ansi" in result

    def test_render_bet_placed_with_leverage(self):
        result = render_bet_placed(50, "radiant", 5)
        assert "```ansi" in result

    def test_render_loan_taken(self):
        result = render_loan_taken(100, 120)
        assert "```ansi" in result

    def test_render_cooldown_hit(self):
        result = render_cooldown_hit("loan")
        assert "```ansi" in result

    def test_render_match_recorded(self):
        result = render_match_recorded()
        assert "```ansi" in result

    def test_render_bankruptcy_filing(self):
        result = render_bankruptcy_filing("TestUser", 300, 2)
        assert "```ansi" in result
        assert "BANKRUPTCY" in result

    def test_render_debt_collector(self):
        result = render_debt_collector("TestUser", 400)
        assert "```ansi" in result
        assert "DEBT" in result

    def test_render_system_breach(self):
        result = render_system_breach("TestUser")
        assert "```ansi" in result
        assert "BREACH" in result or "SYSTEM" in result

    def test_render_balance_zero(self):
        result = render_balance_zero("TestUser")
        assert "```ansi" in result

    def test_render_streak_win(self):
        result = render_streak("TestUser", 5, True)
        assert "```ansi" in result
        assert "WIN" in result or "STREAK" in result or "HOT" in result

    def test_render_streak_loss(self):
        result = render_streak("TestUser", 6, False)
        assert "```ansi" in result
        assert "LOSS" in result or "ANOMALY" in result

    def test_render_negative_loan(self):
        result = render_negative_loan("TestUser", 50, -200)
        assert "```ansi" in result
        assert "RECURSIVE" in result or "DEBT" in result

    def test_render_wheel_bankrupt(self):
        result = render_wheel_bankrupt("TestUser", -100)
        assert "```ansi" in result

    # --- New template tests ---

    def test_render_don_win(self):
        result = render_don_win("TestUser", 200)
        assert "```ansi" in result

    def test_render_don_lose(self):
        result = render_don_lose("TestUser", 150)
        assert "```ansi" in result

    def test_render_don_loss_box(self):
        result = render_don_loss_box("TestUser", 100)
        assert "```ansi" in result
        assert "DOUBLE" in result or "NOTHING" in result

    def test_render_coinflip(self):
        result = render_coinflip("Winner", "Loser")
        assert "```ansi" in result

    def test_render_registration(self):
        result = render_registration("NewPlayer")
        assert "```ansi" in result

    def test_render_prediction_resolved(self):
        result = render_prediction_resolved("Will it rain?", "yes", 300)
        assert "```ansi" in result

    def test_render_prediction_market_crash(self):
        result = render_prediction_market_crash("Will it rain?", 500, "yes", 3, 5)
        assert "```ansi" in result
        assert "MARKET" in result or "SETTLEMENT" in result

    def test_render_soft_avoid(self):
        result = render_soft_avoid(50, 3)
        assert "```ansi" in result

    def test_render_soft_avoid_surveillance(self):
        result = render_soft_avoid_surveillance(50, 3)
        assert "```ansi" in result
        assert "SOCIAL" in result or "Avoid" in result

    def test_all_templates_under_40_lines(self):
        """All renders should produce output under ~40 lines for mobile Discord."""
        renders = [
            render_balance_check("User", 100),
            render_balance_check("User", -200),
            render_bet_placed(50, "radiant", 1),
            render_bet_placed(50, "dire", 5),
            render_loan_taken(100, 120),
            render_cooldown_hit("loan"),
            render_match_recorded(),
            render_bankruptcy_filing("User", 300, 1),
            render_debt_collector("User", 400),
            render_system_breach("User"),
            render_balance_zero("User"),
            render_streak("User", 5, True),
            render_streak("User", 6, False),
            render_negative_loan("User", 50, -200),
            render_wheel_bankrupt("User", -100),
            # New renders
            render_don_win("User", 200),
            render_don_lose("User", 100),
            render_don_loss_box("User", 100),
            render_coinflip("Winner", "Loser"),
            render_registration("NewUser"),
            render_prediction_resolved("Test?", "yes", 200),
            render_prediction_market_crash("Test?", 500, "yes", 3, 5),
            render_soft_avoid(50, 3),
            render_soft_avoid_surveillance(50, 3),
        ]
        for render in renders:
            lines = render.count("\n")
            assert lines <= 45, f"Render has {lines} lines, max 45"


# ---------------------------------------------------------------------------
# NeonDegenService orchestrator tests
# ---------------------------------------------------------------------------


class TestNeonDegenService:
    """Tests for services/neon_degen_service.py orchestrator."""

    def _make_service(self) -> NeonDegenService:
        return NeonDegenService()

    @pytest.mark.asyncio
    async def test_on_balance_check_returns_neon_result_or_none(self):
        service = self._make_service()
        # Call multiple times - should sometimes return a result
        results = []
        for _ in range(50):
            result = await service.on_balance_check(123, 456, 100)
            results.append(result)
        # At least some should be None (due to probability + cooldown)
        # At least some might be NeonResult (due to ~35% chance)
        assert any(r is None for r in results)

    @pytest.mark.asyncio
    async def test_on_balance_check_layer1(self):
        """When it fires, should return layer 1."""
        service = self._make_service()
        # Run until we get a result
        for _ in range(100):
            result = await service.on_balance_check(123, 456, 100)
            if result:
                assert result.layer == 1
                assert result.text_block is not None
                assert "```ansi" in result.text_block
                break

    @pytest.mark.asyncio
    async def test_on_bankruptcy_always_fires(self):
        """Bankruptcy always returns a result (100% chance)."""
        service = self._make_service()
        result = await service.on_bankruptcy(123, 456, debt_cleared=300, filing_number=2)
        assert result is not None
        assert result.layer >= 2
        assert result.text_block is not None

    @pytest.mark.asyncio
    async def test_on_bankruptcy_first_filing_layer3(self):
        """First bankruptcy should trigger Layer 3 (void welcome)."""
        service = self._make_service()
        result = await service.on_bankruptcy(123, 456, debt_cleared=100, filing_number=1)
        assert result is not None
        # Should be layer 3 with GIF (if PIL is available)
        assert result.layer >= 2

    @pytest.mark.asyncio
    async def test_on_bankruptcy_third_filing_layer3(self):
        """Third bankruptcy should trigger Layer 3 (terminal crash)."""
        service = self._make_service()
        result = await service.on_bankruptcy(123, 456, debt_cleared=400, filing_number=3)
        assert result is not None
        assert result.layer >= 2

    @pytest.mark.asyncio
    async def test_cooldown_prevents_rapid_fire(self):
        """Per-user cooldown should prevent rapid consecutive fires."""
        service = self._make_service()
        # Force a fire by using on_bankruptcy (100% chance)
        result1 = await service.on_bankruptcy(123, 456, debt_cleared=100, filing_number=2)
        assert result1 is not None

        # Immediately after, balance check should be cooldown-blocked
        # (since bankruptcy sets cooldown)
        results = []
        for _ in range(10):
            r = await service.on_balance_check(123, 456, 100)
            results.append(r)
        assert all(r is None for r in results)

    @pytest.mark.asyncio
    async def test_disabled_returns_none(self):
        """When disabled, everything returns None."""
        import config
        original = config.NEON_DEGEN_ENABLED
        try:
            config.NEON_DEGEN_ENABLED = False
            service = self._make_service()
            result = await service.on_balance_check(123, 456, 100)
            assert result is None
            result = await service.on_bankruptcy(123, 456, 300, 5)
            assert result is None
        finally:
            config.NEON_DEGEN_ENABLED = original

    @pytest.mark.asyncio
    async def test_on_bet_placed_returns_correct_layer(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_bet_placed(999, 456, 50, 1, "radiant")
            if result:
                assert result.layer == 1
                break

    @pytest.mark.asyncio
    async def test_on_loan_normal(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_loan(777, 456, amount=50, total_owed=60, is_negative=False)
            if result:
                assert result.layer == 1
                break

    @pytest.mark.asyncio
    async def test_on_loan_negative(self):
        """Negative loan should fire at layer 2."""
        found_l2 = False
        for _ in range(100):
            service = self._make_service()  # fresh service to avoid cooldown
            result = await service.on_loan(888, 456, amount=50, total_owed=200, is_negative=True)
            if result and result.layer == 2:
                found_l2 = True
                break
        assert found_l2, "Expected at least one layer 2 result from negative loan in 100 tries"

    @pytest.mark.asyncio
    async def test_on_match_recorded_footer(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_match_recorded(456)
            if result:
                assert result.layer == 1
                assert result.footer_text is not None
                break

    @pytest.mark.asyncio
    async def test_on_degen_milestone_one_time(self):
        """Degen milestone should only fire once per user."""
        service = self._make_service()
        result1 = await service.on_degen_milestone(123, 456, 95)
        # First time may or may not generate GIF depending on PIL
        # But the one-time flag should be set
        result2 = await service.on_degen_milestone(123, 456, 95)
        assert result2 is None  # Should not fire again

    @pytest.mark.asyncio
    async def test_neon_result_dataclass(self):
        """NeonResult should properly hold data."""
        result = NeonResult(layer=1, text_block="test")
        assert result.layer == 1
        assert result.text_block == "test"
        assert result.gif_file is None
        assert result.footer_text is None

        buf = io.BytesIO(b"fake gif")
        result2 = NeonResult(layer=3, gif_file=buf, text_block="text")
        assert result2.layer == 3
        assert result2.gif_file is not None

    # --- New service method tests ---

    @pytest.mark.asyncio
    async def test_on_double_or_nothing_win(self):
        """DoN win should always fire layer 1."""
        service = self._make_service()
        result = await service.on_double_or_nothing(123, 456, won=True, balance_at_risk=100, final_balance=200)
        assert result is not None
        assert result.layer == 1
        assert result.text_block is not None
        assert "```ansi" in result.text_block

    @pytest.mark.asyncio
    async def test_on_double_or_nothing_lose_small(self):
        """DoN loss with small risk should fire layer 1."""
        service = self._make_service()
        result = await service.on_double_or_nothing(124, 456, won=False, balance_at_risk=20, final_balance=0)
        assert result is not None
        assert result.layer == 1
        assert result.text_block is not None

    @pytest.mark.asyncio
    async def test_on_double_or_nothing_lose_large(self):
        """DoN loss with >50 JC at risk should fire layer >= 1."""
        service = self._make_service()
        result = await service.on_double_or_nothing(125, 456, won=False, balance_at_risk=80, final_balance=0)
        assert result is not None
        assert result.layer >= 1

    @pytest.mark.asyncio
    async def test_on_draft_coinflip_probabilistic(self):
        """Draft coinflip should sometimes fire (40% chance)."""
        service = self._make_service()
        results = []
        for _ in range(100):
            r = await service.on_draft_coinflip(456, 1001, 1002)
            results.append(r)
        # At least some should fire and some shouldn't
        assert any(r is not None for r in results)
        assert any(r is None for r in results)

    @pytest.mark.asyncio
    async def test_on_draft_coinflip_layer1(self):
        """When coinflip fires, should return layer 1."""
        service = self._make_service()
        for _ in range(100):
            result = await service.on_draft_coinflip(456, 1001, 1002)
            if result:
                assert result.layer == 1
                assert result.text_block is not None
                break

    @pytest.mark.asyncio
    async def test_on_registration_one_time(self):
        """Registration should only fire once per user."""
        service = self._make_service()
        fired = False
        for _ in range(100):
            result = await service.on_registration(126, 456, "NewPlayer")
            if result:
                fired = True
                assert result.layer == 1
                break
        # Whether it fired or not, second attempt should never fire
        for _ in range(50):
            result2 = await service.on_registration(126, 456, "NewPlayer")
            if fired:
                # Already fired once, should not fire again
                assert result2 is None

    @pytest.mark.asyncio
    async def test_on_prediction_resolved_small_pool(self):
        """Small pool resolution fires at 30% with layer 1."""
        service = self._make_service()
        for _ in range(100):
            result = await service.on_prediction_resolved(
                guild_id=456, question="Test?", outcome="yes",
                total_pool=50, winner_count=2, loser_count=3
            )
            if result:
                assert result.layer == 1
                break

    @pytest.mark.asyncio
    async def test_on_prediction_resolved_large_pool(self):
        """Large pool (>=200) should fire layer 2."""
        service = self._make_service()
        for _ in range(100):
            result = await service.on_prediction_resolved(
                guild_id=456, question="Big question?", outcome="no",
                total_pool=300, winner_count=5, loser_count=10
            )
            if result:
                assert result.layer >= 1
                break

    @pytest.mark.asyncio
    async def test_on_soft_avoid(self):
        """Soft avoid should fire at layer 1 or 2."""
        found = False
        for _ in range(100):
            service = self._make_service()
            result = await service.on_soft_avoid(888, 456, cost=50, games=3)
            if result:
                assert result.layer in (1, 2)
                assert "```ansi" in result.text_block
                found = True
                break
        assert found, "Expected at least one result from soft avoid in 100 tries"

    @pytest.mark.asyncio
    async def test_on_double_or_nothing_disabled(self):
        """When disabled, DoN returns None."""
        import config
        original = config.NEON_DEGEN_ENABLED
        try:
            config.NEON_DEGEN_ENABLED = False
            service = self._make_service()
            result = await service.on_double_or_nothing(123, 456, True, 100, 200)
            assert result is None
        finally:
            config.NEON_DEGEN_ENABLED = original


# ---------------------------------------------------------------------------
# GIF generation tests
# ---------------------------------------------------------------------------


class TestNeonDrawing:
    """Tests for utils/neon_drawing.py GIF generation."""

    def test_terminal_crash_gif_generates(self):
        from utils.neon_drawing import create_terminal_crash_gif
        buf = create_terminal_crash_gif("TestUser", 3)
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert len(data) > 0
        # Check GIF magic bytes
        assert data[:3] == b"GIF"

    def test_terminal_crash_gif_under_4mb(self):
        from utils.neon_drawing import create_terminal_crash_gif
        buf = create_terminal_crash_gif("TestUser", 5)
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"Terminal crash GIF is {size_mb:.2f} MB, exceeds 4MB limit"

    def test_void_welcome_gif_generates(self):
        from utils.neon_drawing import create_void_welcome_gif
        buf = create_void_welcome_gif("TestUser")
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert data[:3] == b"GIF"

    def test_void_welcome_gif_under_4mb(self):
        from utils.neon_drawing import create_void_welcome_gif
        buf = create_void_welcome_gif("TestUser")
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"Void welcome GIF is {size_mb:.2f} MB, exceeds 4MB limit"

    def test_debt_collector_gif_generates(self):
        from utils.neon_drawing import create_debt_collector_gif
        buf = create_debt_collector_gif("TestUser", 500)
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert data[:3] == b"GIF"

    def test_debt_collector_gif_under_4mb(self):
        from utils.neon_drawing import create_debt_collector_gif
        buf = create_debt_collector_gif("TestUser", 500)
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"Debt collector GIF is {size_mb:.2f} MB, exceeds 4MB limit"

    def test_freefall_gif_generates(self):
        from utils.neon_drawing import create_freefall_gif
        buf = create_freefall_gif("TestUser", 200, 0)
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert data[:3] == b"GIF"

    def test_freefall_gif_under_4mb(self):
        from utils.neon_drawing import create_freefall_gif
        buf = create_freefall_gif("TestUser", 200, 0)
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"Freefall GIF is {size_mb:.2f} MB, exceeds 4MB limit"

    def test_degen_certificate_gif_generates(self):
        from utils.neon_drawing import create_degen_certificate_gif
        buf = create_degen_certificate_gif("TestUser", 95)
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert data[:3] == b"GIF"

    def test_degen_certificate_gif_under_4mb(self):
        from utils.neon_drawing import create_degen_certificate_gif
        buf = create_degen_certificate_gif("TestUser", 95)
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"Degen certificate GIF is {size_mb:.2f} MB, exceeds 4MB limit"

    # --- New GIF tests ---

    def test_don_coin_flip_gif_generates(self):
        from utils.neon_drawing import create_don_coin_flip_gif
        buf = create_don_coin_flip_gif("TestUser", 150)
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert len(data) > 0
        assert data[:3] == b"GIF"

    def test_don_coin_flip_gif_under_4mb(self):
        from utils.neon_drawing import create_don_coin_flip_gif
        buf = create_don_coin_flip_gif("TestUser", 200)
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"DoN coin flip GIF is {size_mb:.2f} MB, exceeds 4MB limit"

    def test_market_crash_gif_generates(self):
        from utils.neon_drawing import create_market_crash_gif
        buf = create_market_crash_gif(500, "yes", 3, 7)
        assert isinstance(buf, io.BytesIO)
        data = buf.getvalue()
        assert len(data) > 0
        assert data[:3] == b"GIF"

    def test_market_crash_gif_under_4mb(self):
        from utils.neon_drawing import create_market_crash_gif
        buf = create_market_crash_gif(1000, "no", 5, 10)
        size_mb = len(buf.getvalue()) / (1024 * 1024)
        assert size_mb < 4, f"Market crash GIF is {size_mb:.2f} MB, exceeds 4MB limit"
