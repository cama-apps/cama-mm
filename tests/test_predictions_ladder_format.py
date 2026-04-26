"""Tests for the user-facing ladder + market-field renders in commands/predictions.py."""

from __future__ import annotations

from commands.predictions import _build_ladder_fields, _format_market_field, _trade_link

# --------------------------------------------------------------------------- #
# _build_ladder_fields — buy-only two-column visual ladder
# --------------------------------------------------------------------------- #


def test_ladder_dom_style_no_above_yes_converging_at_mid():
    """Real DOM layout: NO on top with cheapest at bottom of section, mid line,
    YES on bottom with cheapest at top of section. Both sides converge at mid."""
    book = {
        "current_price": 17,
        "yes_asks": [(18, 5), (19, 5), (20, 5)],
        "yes_bids": [(16, 5), (15, 5), (14, 5)],
    }
    fields = _build_ladder_fields(book)
    assert len(fields) == 1
    name, value, inline = fields[0]
    assert name == "Order book"
    assert not inline
    # Buy NO above the mid line; Buy YES below.
    no_idx = value.index("Buy NO")
    mid_idx = value.index("price 17")
    yes_idx = value.index("Buy YES")
    assert no_idx < mid_idx < yes_idx
    # NO section: deepest (most expensive, e.g. 86) at top, cheapest (84) at bottom — closest to mid.
    no_section = value[no_idx:mid_idx]
    assert no_section.index(" 86 ") < no_section.index(" 85 ") < no_section.index(" 84 ")
    # YES section: cheapest (18) at top — closest to mid — deeper (20) at bottom.
    yes_section = value[yes_idx:]
    assert yes_section.index(" 18 ") < yes_section.index(" 19 ") < yes_section.index(" 20 ")
    # Mid line shows both probabilities.
    assert "17% YES" in value and "83% NO" in value
    # No bid/ask jargon, no top arrow, no sell language.
    assert "Sell" not in value
    assert "<- top" not in value
    assert "ASK" not in value and "BID" not in value


def test_ladder_color_codes_yes_green_no_red():
    """Code block declares ansi syntax and rows carry green/red ANSI hooks."""
    book = {
        "current_price": 50,
        "yes_asks": [(51, 5)],
        "yes_bids": [(49, 5)],
    }
    value = _build_ladder_fields(book)[0][1]
    # ANSI code block opens with `ansi` so Discord parses escapes.
    assert value.startswith("```ansi\n")
    # Mobile-safe emoji headers
    assert "🟢 Buy YES" in value
    assert "🔴 Buy NO" in value
    # ANSI green/red/reset escapes are embedded with the actual ESC byte
    # (\x1b) — assert the byte sequence so a missing escape character would
    # fail in CI rather than render as literal text in Discord.
    assert "\x1b[0;32m" in value
    assert "\x1b[0;31m" in value
    assert "\x1b[0;0m" in value


def test_ladder_asymmetric_market_mirror_prices():
    book = {
        "current_price": 17,
        "yes_asks": [(18, 5), (19, 5), (20, 5)],
        "yes_bids": [(16, 5), (15, 5), (14, 5)],
    }
    value = _build_ladder_fields(book)[0][1]
    # YES side prices appear as-is
    for p in (18, 19, 20):
        assert f" {p} " in value or f"{p} " in value or f" {p}" in value
    # NO side mirrors: 100 - YES bid
    for no_price in (84, 85, 86):
        assert str(no_price) in value
    # Mid label shows asymmetric probability
    assert "17% YES" in value
    assert "83% NO" in value


def test_ladder_uses_depth_bars_capped():
    """Each row has a █ bar reflecting size, capped at BAR_CAP=10 chars."""
    book = {
        "current_price": 50,
        "yes_asks": [(51, 3)],
        "yes_bids": [(49, 25)],  # large size — bar should cap at 10
    }
    value = _build_ladder_fields(book)[0][1]
    # 3 blocks for size 3
    assert "███" in value
    # 10-block cap for size 25 (and the actual count "25" still appears)
    assert "██████████" in value
    assert "25" in value


def test_ladder_empty_book():
    fields = _build_ladder_fields({"current_price": 50, "yes_asks": [], "yes_bids": []})
    value = fields[0][1]
    # Both sections labelled empty
    assert value.count("(none — refreshes daily)") == 2
    assert "price 50" in value


def test_ladder_only_asks_keeps_no_section_empty():
    fields = _build_ladder_fields(
        {"current_price": 50, "yes_asks": [(51, 5)], "yes_bids": []}
    )
    value = fields[0][1]
    # YES side has the 51 price; NO side is the empty placeholder.
    assert "51" in value
    assert "(none — refreshes daily)" in value


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
