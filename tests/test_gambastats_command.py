"""
Tests for the /gambastats command with color-coded emoji indicators.
"""

import discord
import pytest

from services.gambling_stats_service import GambaStats, DegenScoreBreakdown
from utils.formatting import JOPACOIN_EMOTE


def create_degen_score(total=25, title="Casual"):
    """Helper to create a DegenScoreBreakdown with all required fields."""
    return DegenScoreBreakdown(
        total=total,
        title=title,
        emoji="🎲",
        tagline="Test degen",
        max_leverage_score=5,
        bet_size_score=5,
        debt_depth_score=0,
        bankruptcy_score=0,
        frequency_score=10,
        loss_chase_score=5,
        negative_loan_bonus=0,
        flavor_texts=[],
    )


def create_gamba_stats(discord_id=456, **kwargs):
    """Helper to create a GambaStats with defaults."""
    defaults = {
        "discord_id": discord_id,
        "total_bets": 10,
        "wins": 7,
        "losses": 3,
        "win_rate": 0.7,
        "net_pnl": 50,
        "roi": 0.20,
        "total_wagered": 100,
        "avg_bet_size": 10.0,
        "leverage_distribution": {1: 8, 2: 2},
        "current_streak": 2,
        "best_streak": 3,
        "worst_streak": -2,
        "peak_pnl": 100,
        "trough_pnl": -30,
        "biggest_win": 50,
        "biggest_loss": -30,
        "matches_played": 10,
        "paper_hands_count": 1,
        "degen_score": create_degen_score(),
    }
    defaults.update(kwargs)
    return GambaStats(**defaults)


class TestGambaStatsEmbedFormatting:
    """Tests for /gambastats embed field formatting with color emojis."""

    def test_positive_pnl_shows_green_emoji_and_plus_sign(self):
        """Test that positive P&L shows green checkmark emoji and plus sign."""
        stats = create_gamba_stats(net_pnl=50, roi=0.20)

        # Simulate the embed field construction from the command
        pnl_str = f"+{stats.net_pnl}" if stats.net_pnl >= 0 else str(stats.net_pnl)
        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"
        pnl_color = "✅" if stats.net_pnl >= 0 else "❌"
        roi_color = "✅" if stats.roi >= 0 else "❌"

        performance_field = (
            f"**Net P&L:** {pnl_color} {pnl_str} {JOPACOIN_EMOTE}\n"
            f"**ROI:** {roi_color} {roi_str}\n"
            f"**Record:** {stats.wins}W-{stats.losses}L ({stats.win_rate:.0%})"
        )

        # Assertions
        assert "✅" in performance_field, "Green checkmark missing for positive P&L"
        assert "+50" in performance_field, "Positive P&L value not formatted"
        assert "+20.0%" in performance_field, "Positive ROI not formatted"

    def test_negative_pnl_shows_red_emoji_and_minus_sign(self):
        """Test that negative P&L shows red X emoji and minus sign."""
        stats = create_gamba_stats(
            total_bets=20,
            wins=6,
            losses=14,
            win_rate=0.3,
            net_pnl=-150,
            roi=-0.45,
        )

        # Simulate the embed field construction from the command
        pnl_str = f"+{stats.net_pnl}" if stats.net_pnl >= 0 else str(stats.net_pnl)
        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"
        pnl_color = "✅" if stats.net_pnl >= 0 else "❌"
        roi_color = "✅" if stats.roi >= 0 else "❌"

        performance_field = (
            f"**Net P&L:** {pnl_color} {pnl_str} {JOPACOIN_EMOTE}\n"
            f"**ROI:** {roi_color} {roi_str}\n"
            f"**Record:** {stats.wins}W-{stats.losses}L ({stats.win_rate:.0%})"
        )

        # Assertions
        assert "❌" in performance_field, "Red X missing for negative P&L"
        assert "-150" in performance_field, "Negative P&L value not formatted"
        assert "-45.0%" in performance_field, "Negative ROI not formatted"

    def test_extremes_field_color_coding_positive_values(self):
        """Test that Extremes field has green checkmark for positive values."""
        stats = create_gamba_stats(
            peak_pnl=150,
            trough_pnl=-80,
            biggest_win=75,
            biggest_loss=-60,
        )

        # Simulate the embed field construction from the command
        peak_str = f"+{stats.peak_pnl}" if stats.peak_pnl > 0 else str(stats.peak_pnl)
        biggest_win_str = f"+{stats.biggest_win}" if stats.biggest_win > 0 else "None"

        peak_emoji = "✅" if stats.peak_pnl > 0 else "❌"
        win_emoji = "✅" if stats.biggest_win > 0 else "➖"

        extremes_field = (
            f"**Peak:** {peak_emoji} {peak_str} {JOPACOIN_EMOTE}\n"
            f"**Best Win:** {win_emoji} {biggest_win_str} {JOPACOIN_EMOTE}"
        )

        # Assertions
        assert "**Peak:** ✅" in extremes_field, "Peak should have green checkmark"
        assert "+150" in extremes_field, "Peak P&L value not formatted"
        assert "**Best Win:** ✅" in extremes_field, "Best Win should have green checkmark"
        assert "+75" in extremes_field, "Best Win value not formatted"

    def test_extremes_field_color_coding_negative_values(self):
        """Test that Extremes field has red X for negative values."""
        stats = create_gamba_stats(
            peak_pnl=150,
            trough_pnl=-200,
            biggest_win=75,
            biggest_loss=-80,
        )

        # Simulate the embed field construction from the command
        trough_str = str(stats.trough_pnl)
        biggest_loss_str = str(stats.biggest_loss) if stats.biggest_loss < 0 else "None"

        trough_emoji = "❌" if stats.trough_pnl < 0 else "✅"
        loss_emoji = "❌" if stats.biggest_loss < 0 else "➖"

        extremes_field = (
            f"**Trough:** {trough_emoji} {trough_str} {JOPACOIN_EMOTE}\n"
            f"**Worst Loss:** {loss_emoji} {biggest_loss_str} {JOPACOIN_EMOTE}"
        )

        # Assertions
        assert "**Trough:** ❌" in extremes_field, "Trough should have red X"
        assert "-200" in extremes_field, "Trough value not formatted"
        assert "**Worst Loss:** ❌" in extremes_field, "Worst Loss should have red X"
        assert "-80" in extremes_field, "Worst Loss value not formatted"

    def test_zero_values_show_neutral_emoji(self):
        """Test that zero/None values show neutral emoji."""
        stats = create_gamba_stats(
            peak_pnl=0,
            biggest_win=0,
            biggest_loss=0,
        )

        # For zero peak
        peak_emoji = "✅" if stats.peak_pnl > 0 else "❌"
        # For zero wins
        win_emoji = "✅" if stats.biggest_win > 0 else "➖"
        # For zero losses
        loss_emoji = "❌" if stats.biggest_loss < 0 else "➖"

        # Assertions - zero values should not get green or red for wins/losses
        assert peak_emoji == "❌", "Zero peak should show red X"
        assert win_emoji == "➖", "Zero biggest win should show neutral emoji"
        assert loss_emoji == "➖", "Zero biggest loss should show neutral emoji"

    def test_embed_color_green_for_positive_pnl(self):
        """Test that embed color is green (0x57F287) for positive P&L."""
        stats = create_gamba_stats(net_pnl=100)

        # Simulate color selection logic
        pnl_color = 0x57F287 if stats.net_pnl >= 0 else 0xED4245

        assert pnl_color == 0x57F287, "Positive P&L should result in green color"

    def test_embed_color_red_for_negative_pnl(self):
        """Test that embed color is red (0xED4245) for negative P&L."""
        stats = create_gamba_stats(net_pnl=-100)

        # Simulate color selection logic
        pnl_color = 0x57F287 if stats.net_pnl >= 0 else 0xED4245

        assert pnl_color == 0xED4245, "Negative P&L should result in red color"

    def test_roi_formatting_positive(self):
        """Test ROI formatting for positive values."""
        stats = create_gamba_stats(roi=0.25)

        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"

        assert roi_str == "+25.0%", "Positive ROI should have plus sign and percentage"

    def test_roi_formatting_negative(self):
        """Test ROI formatting for negative values."""
        stats = create_gamba_stats(roi=-0.35)

        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"

        assert roi_str == "-35.0%", "Negative ROI should have minus sign and percentage"

    def test_full_performance_field_positive_pnl(self):
        """Test complete Performance field with positive P&L."""
        stats = create_gamba_stats(
            net_pnl=75,
            roi=0.15,
            wins=8,
            losses=2,
            win_rate=0.8,
        )

        pnl_str = f"+{stats.net_pnl}" if stats.net_pnl >= 0 else str(stats.net_pnl)
        roi_str = f"+{stats.roi:.1%}" if stats.roi >= 0 else f"{stats.roi:.1%}"
        pnl_color = "✅" if stats.net_pnl >= 0 else "❌"
        roi_color = "✅" if stats.roi >= 0 else "❌"

        performance_field = (
            f"**Net P&L:** {pnl_color} {pnl_str} {JOPACOIN_EMOTE}\n"
            f"**ROI:** {roi_color} {roi_str}\n"
            f"**Record:** {stats.wins}W-{stats.losses}L ({stats.win_rate:.0%})"
        )

        # Verify all components are present
        assert "**Net P&L:** ✅ +75" in performance_field
        assert "**ROI:** ✅ +15.0%" in performance_field
        assert "**Record:** 8W-2L (80%)" in performance_field
        assert JOPACOIN_EMOTE in performance_field

    def test_full_extremes_field_mixed_values(self):
        """Test complete Extremes field with mixed positive/negative values."""
        stats = create_gamba_stats(
            peak_pnl=200,
            trough_pnl=-150,
            biggest_win=100,
            biggest_loss=-80,
        )

        peak_str = f"+{stats.peak_pnl}" if stats.peak_pnl > 0 else str(stats.peak_pnl)
        trough_str = str(stats.trough_pnl)
        biggest_win_str = f"+{stats.biggest_win}" if stats.biggest_win > 0 else "None"
        biggest_loss_str = str(stats.biggest_loss) if stats.biggest_loss < 0 else "None"

        peak_emoji = "✅" if stats.peak_pnl > 0 else "❌"
        trough_emoji = "❌" if stats.trough_pnl < 0 else "✅"
        win_emoji = "✅" if stats.biggest_win > 0 else "➖"
        loss_emoji = "❌" if stats.biggest_loss < 0 else "➖"

        extremes_field = (
            f"**Peak:** {peak_emoji} {peak_str} {JOPACOIN_EMOTE}\n"
            f"**Trough:** {trough_emoji} {trough_str} {JOPACOIN_EMOTE}\n"
            f"**Best Win:** {win_emoji} {biggest_win_str} {JOPACOIN_EMOTE}\n"
            f"**Worst Loss:** {loss_emoji} {biggest_loss_str} {JOPACOIN_EMOTE}"
        )

        # Verify all components
        assert "**Peak:** ✅ +200" in extremes_field
        assert "**Trough:** ❌ -150" in extremes_field
        assert "**Best Win:** ✅ +100" in extremes_field
        assert "**Worst Loss:** ❌ -80" in extremes_field
