"""Tests for the user-facing ladder + market-field renders in commands/predictions.py."""

from __future__ import annotations

from commands.predictions import _build_ladder_fields, _format_market_field, _trade_link

# --------------------------------------------------------------------------- #
# _build_ladder_fields — buy-only two-column visual ladder
# --------------------------------------------------------------------------- #


def test_ladder_dom_style_yes_above_no_converging_at_mid():
    """Real DOM layout: YES on top with cheapest at bottom of section, mid line,
    NO on bottom with cheapest at top of section. Both sides converge at mid —
    so buying YES (consuming the cheapest YES near the mid) drives the mid up
    into the YES stack, mirroring a stock DOM."""
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
    # Buy YES above the mid line; Buy NO below.
    yes_idx = value.index("Buy YES")
    mid_idx = value.index("YES 18%")
    no_idx = value.index("Buy NO")
    assert yes_idx < mid_idx < no_idx
    # YES section: deepest (most expensive, e.g. 20) at top, cheapest (18) at bottom — closest to mid.
    # Use the exact _row emit pattern ("  {price:>3}  ") to avoid loose substring matches.
    yes_section = value[yes_idx:mid_idx]
    assert yes_section.index("  20  ") < yes_section.index("  19  ") < yes_section.index("  18  ")
    # NO section: cheapest (84) at top — closest to mid — deeper (86) at bottom.
    no_section = value[no_idx:]
    assert no_section.index("  84  ") < no_section.index("  85  ") < no_section.index("  86  ")
    # Mid line shows actual cheapest tradeable prices on each side.
    assert "YES 18%" in value and "NO 84%" in value
    # No bid/ask jargon, no top arrow, no sell language, no stale "price X" label.
    assert "Sell" not in value
    assert "<- top" not in value
    assert "ASK" not in value and "BID" not in value
    assert "price 17" not in value


def test_ladder_dom_order_is_yes_above_no_for_symmetric_market():
    """At fair=50 the cheapest YES (51) and cheapest NO (51) coincide, but the
    YES section still sits above the mid line and NO sits below — guards
    against an accidental swap when both sides land on the same number."""
    book = {
        "current_price": 50,
        "yes_asks": [(51, 5)],
        "yes_bids": [(49, 5)],
    }
    value = _build_ladder_fields(book)[0][1]
    assert value.index("Buy YES") < value.index("YES 51%") < value.index("Buy NO")


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
    # Mid line shows the asymmetric cheapest-side tradeable prices.
    assert "YES 18%" in value
    assert "NO 84%" in value


def test_ladder_depth_bars_scale_per_ten_size():
    """Bars render at 1 cell per 10 size units. Sub-10 sizes render empty
    (the right-aligned size column still surfaces actual depth); 100+ caps
    at BAR_CAP=10 cells."""
    book = {
        "current_price": 50,
        "yes_asks": [(60, 3), (61, 25), (62, 80), (63, 250)],
        "yes_bids": [],
    }
    value = _build_ladder_fields(book)[0][1]

    def _row_for(price: int) -> str:
        rows = [r for r in value.splitlines() if f"  {price:>3}  " in r]
        assert rows, f"no row found for price {price}"
        return rows[0]

    # size 3 → no █ on that row.
    assert "█" not in _row_for(60)
    # size 25 → exactly two cells, not three.
    row_25 = _row_for(61)
    assert "██" in row_25 and "███" not in row_25
    # size 80 → eight cells, not nine.
    row_80 = _row_for(62)
    assert "████████" in row_80 and "█████████" not in row_80
    # size 250 → caps at ten cells.
    row_250 = _row_for(63)
    assert "██████████" in row_250 and "███████████" not in row_250


def test_ladder_empty_book():
    fields = _build_ladder_fields({"current_price": 50, "yes_asks": [], "yes_bids": []})
    value = fields[0][1]
    # Both sections labelled empty
    assert value.count("(none — refreshes daily)") == 2
    # Mid line honestly shows no tradeable prices on either side.
    assert "YES —" in value and "NO —" in value
    # The stale "price 50" label must be gone — the bot's fair belief lives in
    # the Current field, not in the order book mid line.
    assert "price 50" not in value


def test_ladder_only_asks_keeps_no_section_empty():
    fields = _build_ladder_fields(
        {"current_price": 50, "yes_asks": [(51, 5)], "yes_bids": []}
    )
    value = fields[0][1]
    # YES side has the 51 price; NO side is the empty placeholder.
    assert "51" in value
    assert "(none — refreshes daily)" in value
    # Mid line is asymmetric: YES has a tradeable price, NO does not.
    assert "YES 51%" in value and "NO —" in value


def test_ladder_handles_missing_current_price():
    fields = _build_ladder_fields({"current_price": None, "yes_asks": [], "yes_bids": []})
    value = fields[0][1]
    # Nothing tradeable and no fair belief — mid line falls back to a bare "?".
    assert "?" in value
    assert "price" not in value


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
    assert "YES 17%" in name
    assert "price" not in name
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
    assert "YES 19%" in name
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
    assert "YES 50%" in name
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
