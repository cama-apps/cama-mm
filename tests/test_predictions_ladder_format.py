"""Tests for the user-facing ladder + market-field renders in commands/predictions.py."""

from __future__ import annotations

from commands.predictions import _build_ladder_fields, _format_market_field, _trade_link

# --------------------------------------------------------------------------- #
# _build_ladder_fields — replaces the old code-block ladder
# --------------------------------------------------------------------------- #


def _by_name(fields):
    return {name: value for name, value, _inline in fields}


def test_ladder_fields_balanced_book():
    book = {
        "current_price": 50,
        "yes_asks": [(51, 5), (52, 5), (53, 5)],
        "yes_bids": [(49, 5), (48, 5), (47, 5)],
    }
    fields = _build_ladder_fields(book)
    assert len(fields) == 4
    # All four are inline so Discord renders them as a 2x2 grid.
    assert all(inline for _, _, inline in fields)
    by_name = _by_name(fields)
    # Buy YES uses the asks ascending: 51, 52, 53
    assert by_name["Buy YES"] == "51 x5 / 52 x5 / 53 x5"
    # Buy NO mirrors top YES bid: 100-49=51, 100-48=52, 100-47=53
    assert by_name["Buy NO"] == "51 x5 / 52 x5 / 53 x5"
    # Sell YES uses bids descending (best first): 49, 48, 47
    assert by_name["Sell YES"] == "49 x5 / 48 x5 / 47 x5"
    # Sell NO mirrors asks: 100-51=49, 100-52=48, 100-53=47
    assert by_name["Sell NO"] == "49 x5 / 48 x5 / 47 x5"


def test_ladder_fields_asymmetric_market():
    book = {
        "current_price": 17,
        "yes_asks": [(18, 5), (19, 5), (20, 5)],
        "yes_bids": [(16, 5), (15, 5), (14, 5)],
    }
    by_name = _by_name(_build_ladder_fields(book))
    assert by_name["Buy YES"] == "18 x5 / 19 x5 / 20 x5"
    assert by_name["Buy NO"] == "84 x5 / 85 x5 / 86 x5"
    assert by_name["Sell YES"] == "16 x5 / 15 x5 / 14 x5"
    assert by_name["Sell NO"] == "82 x5 / 81 x5 / 80 x5"


def test_ladder_fields_empty_asks():
    book = {"current_price": 50, "yes_asks": [], "yes_bids": [(49, 5)]}
    by_name = _by_name(_build_ladder_fields(book))
    assert by_name["Buy YES"] == "(none)"
    assert by_name["Sell NO"] == "(none)"
    assert by_name["Buy NO"] == "51 x5"
    assert by_name["Sell YES"] == "49 x5"


def test_ladder_fields_empty_bids():
    book = {"current_price": 50, "yes_asks": [(51, 5)], "yes_bids": []}
    by_name = _by_name(_build_ladder_fields(book))
    assert by_name["Buy YES"] == "51 x5"
    assert by_name["Buy NO"] == "(none)"
    assert by_name["Sell YES"] == "(none)"
    assert by_name["Sell NO"] == "49 x5"


def test_ladder_fields_fully_empty_book():
    fields = _build_ladder_fields({"current_price": 50, "yes_asks": [], "yes_bids": []})
    by_name = _by_name(fields)
    assert all(v == "(none)" for v in by_name.values())


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
    # Mention markup is preserved verbatim — Discord resolves at render time.
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
