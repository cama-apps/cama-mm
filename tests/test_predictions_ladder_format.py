"""Tests for the user-facing ladder render in commands/predictions.py."""

from __future__ import annotations

from commands.predictions import _format_ladder


def test_format_ladder_balanced_book():
    book = {
        "current_price": 50,
        "yes_asks": [(51, 5), (52, 5), (53, 5)],
        "yes_bids": [(49, 5), (48, 5), (47, 5)],
    }
    out = _format_ladder(book)
    # Each user-facing action is labelled with its real price.
    assert "Buy YES:" in out
    assert "Buy NO:" in out
    assert "Sell YES:" in out
    assert "Sell NO:" in out
    # Buy YES uses YES asks: 51, 52, 53
    assert "51 x5" in out
    # Buy NO uses 100 - YES bid: 49 → 51
    assert "51 x5" in out  # already asserted above, but mirror lands in same string
    # Sell YES uses YES bids: 49
    assert "49 x5" in out
    # Sell NO uses 100 - YES ask: 51 → 49 (same)
    # current_price shown
    assert "price: 50" in out
    # No ASK/BID jargon
    assert "ASK" not in out and "BID" not in out


def test_format_ladder_asymmetric_market():
    """At fair=17, NO is much more expensive than YES — verify the mirror math."""
    book = {
        "current_price": 17,
        "yes_asks": [(18, 5), (19, 5), (20, 5)],
        "yes_bids": [(16, 5), (15, 5), (14, 5)],
    }
    out = _format_ladder(book)
    # Buy YES top of book: 18
    assert "18 x5" in out
    # Buy NO top of book: 100 - 16 = 84
    assert "84 x5" in out
    # Sell YES top: 16
    assert "16 x5" in out
    # Sell NO top: 100 - 18 = 82
    assert "82 x5" in out
    # Deeper levels too
    assert "85 x5" in out and "86 x5" in out  # buy NO depth
    assert "81 x5" in out and "80 x5" in out  # sell NO depth


def test_format_ladder_handles_empty_asks():
    book = {
        "current_price": 50,
        "yes_asks": [],
        "yes_bids": [(49, 5)],
    }
    out = _format_ladder(book)
    # Buy YES has nothing to do
    assert "Buy YES:" in out
    assert "(none)" in out
    # Buy NO can still happen (mirror of bids): 100 - 49 = 51
    assert "51 x5" in out


def test_format_ladder_handles_empty_bids():
    book = {
        "current_price": 50,
        "yes_asks": [(51, 5)],
        "yes_bids": [],
    }
    out = _format_ladder(book)
    # Buy YES at 51 still works
    assert "51 x5" in out
    # Buy NO has nothing (no bids to mirror); Sell YES has nothing.
    # Verify (none) appears for the empty rows.
    assert "(none)" in out


def test_format_ladder_handles_fully_empty_book():
    book = {"current_price": 50, "yes_asks": [], "yes_bids": []}
    out = _format_ladder(book)
    assert "Buy YES:" in out
    assert "(none)" in out
    assert "price: 50" in out


def test_format_ladder_handles_missing_current_price():
    book = {"current_price": None, "yes_asks": [], "yes_bids": []}
    out = _format_ladder(book)
    assert "price: ?" in out
