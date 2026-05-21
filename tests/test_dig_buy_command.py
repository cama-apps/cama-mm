"""Regression + contract tests for the `/dig buy` slash command's item choices.

Bug guarded: amulets were listed in `/dig shop` but the `/dig buy` command had
no amulet option to select, so players could not buy them. TEST 1 pins the
three amulet choices directly. TEST 2 ties the shop's amulet inventory to the
buy command's choices, proving the user-facing contract that anything shown in
the shop is selectable in buy.
"""

import commands.dig as m
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_service import DigService


def _buy_choice_values() -> set[str]:
    """Pull the set of `item` choice values from the `/dig buy` command."""
    grp = m.DigCommands.dig  # an app_commands.Group
    buy = next(c for c in grp.walk_commands() if c.name == "buy")
    item_param = next(p for p in buy.parameters if p.name == "item")
    return {ch.value for ch in item_param.choices}


def _buy_item_param():
    grp = m.DigCommands.dig
    buy = next(c for c in grp.walk_commands() if c.name == "buy")
    return next(p for p in buy.parameters if p.name == "item")


def test_buy_command_offers_amulet_choices():
    """The buy command must expose amulet tiers 1-3 as selectable choices.

    This is the direct regression guard: the bug was that these values were
    absent from the `@app_commands.choices(item=[...])` list.
    """
    item_param = _buy_item_param()
    values = {ch.value for ch in item_param.choices}

    assert "amulet:1" in values
    assert "amulet:2" in values
    assert "amulet:3" in values

    # Labels (Stone Pendant / Iron Talisman / Diamond Charm) must be real,
    # non-empty strings so the Discord choice renders sensibly.
    amulet_labels = {
        ch.name for ch in item_param.choices if ch.value.startswith("amulet:")
    }
    assert len(amulet_labels) == 3
    for label in amulet_labels:
        assert isinstance(label, str)
        assert label.strip()


def test_shop_amulets_are_buyable(repo_db_path):
    """Contract: every amulet tier the shop sells is selectable in buy.

    Builds a funded, registered player with an advanced tunnel, reads the live
    shop, then asserts the shop sells amulet tiers 1-3 and that each one has a
    matching `amulet:<tier>` choice in the buy command. This couples the two
    surfaces so a future divergence (shop adds/removes a tier without updating
    buy) fails loudly.
    """
    drepo = DigRepository(repo_db_path)
    prepo = PlayerRepository(repo_db_path)
    svc = DigService(drepo, prepo)

    prepo.add(discord_id=111, discord_username="pf", guild_id=0)
    prepo.add_balance(111, 0, 5000)
    drepo.create_tunnel(111, 0, "T")
    drepo.update_tunnel(111, 0, depth=100, prestige_level=1)

    shop = svc.get_shop(111, 0)
    gear = shop["gear_for_sale"]
    amulet_tiers = {g["tier"] for g in gear if g["slot"] == "amulet"}

    # The shop sells amulet tiers 1-3 (Stone/Iron/Diamond); tier 0 is the free
    # starter and Obsidian+ are drop-only.
    assert amulet_tiers == {1, 2, 3}

    buy_values = _buy_choice_values()
    for tier in amulet_tiers:
        assert f"amulet:{tier}" in buy_values
