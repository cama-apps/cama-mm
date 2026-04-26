"""Tests for the user-facing ladder + market-field renders in commands/predictions.py."""

from __future__ import annotations

from commands.predictions import _build_ladder_fields, _format_market_field, _trade_link

# --------------------------------------------------------------------------- #
# _build_ladder_fields — buy-only two-column visual ladder
# --------------------------------------------------------------------------- #


def test_ladder_one_field_with_two_buy_columns():
    book = {
        "current_price": 50,
        "yes_asks": [(51, 5), (52, 5), (53, 5)],
        "yes_bids": [(49, 5), (48, 5), (47, 5)],
    }
    fields = _build_ladder_fields(book)
    # Single field (sell side removed — positions are hold-to-resolution)
    assert len(fields) == 1
    name, value, inline = fields[0]
    assert "Buy only" in name
    assert not inline
    # Header column titles
    assert "Buy YES" in value
    assert "Buy NO" in value
    # Top of book = cheapest. YES ask 51, NO ask = 100 - 49 = 51.
    assert "51  x5" in value
    # Mid line shown
    assert "price 50" in value
    # No sell language anywhere
    assert "Sell" not in value


def test_ladder_asymmetric_market():
    book = {
        "current_price": 17,
        "yes_asks": [(18, 5), (19, 5), (20, 5)],
        "yes_bids": [(16, 5), (15, 5), (14, 5)],
    }
    value = _build_ladder_fields(book)[0][1]
    # Buy YES options
    assert "18  x5" in value
    assert "19  x5" in value
    assert "20  x5" in value
    # Buy NO mirror options: 100 - 16 = 84, 100 - 15 = 85, 100 - 14 = 86
    assert "84  x5" in value
    assert "85  x5" in value
    assert "86  x5" in value
    assert "price 17" in value


def test_ladder_empty_book():
    fields = _build_ladder_fields({"current_price": 50, "yes_asks": [], "yes_bids": []})
    value = fields[0][1]
    assert "book empty" in value
    assert "price 50" in value


def test_ladder_only_asks():
    fields = _build_ladder_fields({"current_price": 50, "yes_asks": [(51, 5)], "yes_bids": []})
    value = fields[0][1]
    assert "51  x5" in value
    # No NO column when bids empty
    assert "  -" in value  # placeholder for empty NO column


def test_ladder_handles_missing_current_price():
    fields = _build_ladder_fields({"current_price": None, "yes_asks": [], "yes_bids": []})
    assert "price ?" in fields[0][1]


# --------------------------------------------------------------------------- #
# _trade_link — discord deep link to the trade-buttons message
# --------------------------------------------------------------------------- #


def test_trade_link_with_full_ids_lands_on_message():
    pred = {"guild_id": 1, "thread_id": 2, "embed_message_id": 3}
    assert _trade_link(pred) == "https://discord.com/channels/1/2/3"


def test_trade_link_falls_back_to_thread_when_message_missing():
    pred = {"guild_id": 1, "thread_id": 2, "embed_message_id": None}
    assert _trade_link(pred) == "https://discord.com/channels/1/2"


def test_trade_link_returns_none_when_thread_missing():
    pred = {"guild_id": 1, "thread_id": None, "embed_message_id": 3}
    assert _trade_link(pred) is None


# --------------------------------------------------------------------------- #
# _format_market_field — used by /predict list and the daily digest
# --------------------------------------------------------------------------- #


def test_format_market_field_basic():
    pred = {
        "prediction_id": 5,
        "question": "Will @Luke hit immortal?",
        "current_price": 17,
        "guild_id": 1,
        "thread_id": 2,
        "embed_message_id": 3,
    }
    name, value = _format_market_field(pred)
    assert "📈 #5" in name
    assert "price 17" in name
    assert "@Luke" in value
    assert "[Trade →](https://discord.com/channels/1/2/3)" in value


def test_format_market_field_with_delta_up():
    pred = {
        "prediction_id": 5,
        "question": "x?",
        "current_price": 19,
        "prev_price": 17,
        "guild_id": 1,
        "thread_id": 2,
        "embed_message_id": 3,
    }
    name, _ = _format_market_field(pred, with_delta=True)
    assert "↑2 today" in name


def test_format_market_field_with_delta_down():
    pred = {
        "prediction_id": 6,
        "question": "y?",
        "current_price": 50,
        "prev_price": 53,
        "guild_id": 1,
        "thread_id": 2,
        "embed_message_id": 3,
    }
    name, _ = _format_market_field(pred, with_delta=True)
    assert "↓3 today" in name


def test_format_market_field_with_delta_no_movement_omits_arrow():
    pred = {
        "prediction_id": 7,
        "question": "z?",
        "current_price": 50,
        "prev_price": 50,
        "guild_id": 1,
        "thread_id": 2,
        "embed_message_id": 3,
    }
    name, _ = _format_market_field(pred, with_delta=True)
    assert "↑" not in name and "↓" not in name


def test_format_market_field_omits_link_when_thread_missing():
    pred = {
        "prediction_id": 8,
        "question": "no thread yet",
        "current_price": 50,
        "guild_id": 1,
        "thread_id": None,
        "embed_message_id": None,
    }
    _, value = _format_market_field(pred)
    assert "Trade" not in value
