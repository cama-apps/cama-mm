"""Tests for formatting utilities."""

import pytest

from utils.formatting import JOPACOIN_EMOTE, calculate_pool_odds, format_betting_display


class TestPoolOdds:
    """Tests for pool odds calculation."""

    def test_calculate_pool_odds_even_split(self):
        """Equal bets on both sides results in 2x odds each."""
        radiant_mult, dire_mult = calculate_pool_odds(100, 100)
        assert radiant_mult == 2.0
        assert dire_mult == 2.0

    def test_calculate_pool_odds_underdog(self):
        """Underdog side gets better odds."""
        radiant_mult, dire_mult = calculate_pool_odds(100, 300)
        # Radiant (underdog): 400/100 = 4.0x
        # Dire (favorite): 400/300 = 1.33x
        assert radiant_mult == 4.0
        assert pytest.approx(dire_mult, 0.01) == 1.33

    def test_calculate_pool_odds_empty_side(self):
        """Empty side returns None for that side's multiplier."""
        radiant_mult, dire_mult = calculate_pool_odds(100, 0)
        assert radiant_mult == 1.0  # Gets their own money back
        assert dire_mult is None

        radiant_mult, dire_mult = calculate_pool_odds(0, 100)
        assert radiant_mult is None
        assert dire_mult == 1.0

    def test_calculate_pool_odds_both_empty(self):
        """Both empty returns None for both."""
        radiant_mult, dire_mult = calculate_pool_odds(0, 0)
        assert radiant_mult is None
        assert dire_mult is None


class TestBettingDisplay:
    """Tests for betting display formatting."""

    def test_format_house_mode(self):
        """House mode shows simple totals without odds."""
        field_name, field_value = format_betting_display(100, 200, "house")
        assert field_name == "💰 House Betting (1:1)"
        assert "100" in field_value
        assert "200" in field_value
        assert JOPACOIN_EMOTE in field_value
        # No multipliers in house mode
        assert "x)" not in field_value

    def test_format_pool_mode(self):
        """Pool mode shows totals with odds."""
        field_name, field_value = format_betting_display(100, 200, "pool")
        assert field_name == "💰 Pool Betting"
        assert "100" in field_value
        assert "200" in field_value
        assert JOPACOIN_EMOTE in field_value
        # Pool mode shows multipliers
        assert "(3.00x)" in field_value  # Radiant odds: 300/100 = 3x
        assert "(1.50x)" in field_value  # Dire odds: 300/200 = 1.5x

    def test_format_pool_mode_empty_side(self):
        """Pool mode with empty side shows dash."""
        field_name, field_value = format_betting_display(0, 100, "pool")
        assert "(—)" in field_value  # Radiant has no bets

    def test_format_with_lock_time(self):
        """Lock time is included in the display."""
        lock_ts = 1704067200  # Some timestamp
        field_name, field_value = format_betting_display(100, 100, "house", lock_ts)
        assert f"<t:{lock_ts}:R>" in field_value

    def test_format_pool_mode_equal_bets(self):
        """Pool mode with equal bets shows 2x for both."""
        field_name, field_value = format_betting_display(100, 100, "pool")
        assert "(2.00x)" in field_value
