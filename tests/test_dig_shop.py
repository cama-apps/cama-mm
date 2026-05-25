"""Tests for /dig shop: the get_shop data path and the handler's send guarantee.

Why this exists: there was no coverage of get_shop or the shop handler, which is
how a regression that left /dig shop stuck on "thinking…" forever shipped. These
tests pin (1) that get_shop returns a valid, renderable shop for every player
state and (2) that the handler still delivers the shop to the user even when the
public followup send is rejected by Discord.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import discord
import pytest

from commands.dig import DigCommands
from repositories.dig_repository import DigRepository
from services.dig_data.aliases import CONSUMABLE_ITEMS
from services.dig_service import DigService
from utils.embed_safety import add_lines_field, validate_embed
from utils.formatting import JOPACOIN_EMOTE


def _http_error(status: int = 403, reason: str = "Forbidden", message: str = "Missing Permissions"):
    return discord.HTTPException(SimpleNamespace(status=status, reason=reason), message)


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register(player_repository, discord_id, guild_id, balance=10_000):
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"P{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(discord_id, guild_id, balance)


# ---------------------------------------------------------------------------
# get_shop data path
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("depth,tier", [(0, 0), (80, 3), (300, 7)])
def test_get_shop_succeeds_for_every_player_state(
    dig_service, dig_repo, player_repository, guild_id, depth, tier
):
    """Fresh, mid, and near-prestige players all get a valid shop. The gear rows
    must carry positive integer prices — a None price here (the field the handler
    formats) would break rendering."""
    did = 4000 + tier
    _register(player_repository, did, guild_id)
    dig_repo.create_tunnel(did, guild_id, "T")
    dig_repo.update_tunnel(did, guild_id, depth=depth, pickaxe_tier=tier)

    shop = dig_service.get_shop(did, guild_id)

    assert shop["success"] is True
    assert len(shop["consumables"]) > 0
    assert len(shop["gear_for_sale"]) > 0
    # Every gear/upgrade row carries the well-typed fields the handler formats
    # (a None price/req here is exactly what would break rendering).
    for gear in shop["gear_for_sale"]:
        assert isinstance(gear["price"], int) and gear["price"] > 0
        assert isinstance(gear["depth_req"], int)
        assert isinstance(gear["prestige_req"], int)
    # Higher pickaxe tiers simply expose fewer upgrade rows; never negative/huge.
    assert 0 <= len(shop["pickaxe_upgrades"]) <= 7
    for upgrade in shop["pickaxe_upgrades"]:
        assert isinstance(upgrade["price"], int)
        assert isinstance(upgrade["depth_req"], int)
        assert isinstance(upgrade["prestige_req"], int)
    assert shop["inventory_count"] == 0


def test_get_shop_without_tunnel_still_succeeds(dig_service, player_repository, guild_id):
    """A registered player who has never dug (no tunnel row) can still open the shop."""
    _register(player_repository, 4099, guild_id)

    shop = dig_service.get_shop(4099, guild_id)

    assert shop["success"] is True
    assert len(shop["consumables"]) > 0


# ---------------------------------------------------------------------------
# Handler: the user always gets the shop, even when the public send is rejected
# ---------------------------------------------------------------------------

class _PublicSendRejectingFollowup:
    """A followup whose public send is rejected but whose ephemeral send works —
    mirrors the suspected prod failure where the public post is refused."""

    def __init__(self):
        self.calls: list[dict] = []

    async def send(self, **kwargs):
        self.calls.append(kwargs)
        if not kwargs.get("ephemeral"):
            raise _http_error()
        return "ephemeral-ok"


@pytest.mark.asyncio
async def test_dig_shop_handler_falls_back_to_ephemeral_when_public_send_fails(monkeypatch):
    import config
    import utils.dig_assets

    # Route the channel gate through the gamba check, which the mock channel passes.
    monkeypatch.setattr(config, "DIG_CHANNEL_ID", None)
    # The decorative grid image is irrelevant here; skip rendering it.
    monkeypatch.setattr(utils.dig_assets, "compose_shop_grid", lambda: None)

    followup = _PublicSendRejectingFollowup()
    interaction = SimpleNamespace(
        id=1,
        guild=SimpleNamespace(id=99, get_channel=lambda _id: SimpleNamespace()),
        user=SimpleNamespace(id=42),
        channel=SimpleNamespace(
            name="gamba", parent=None, id=100,
            send=AsyncMock(side_effect=_http_error()),
        ),
        response=SimpleNamespace(defer=AsyncMock(), is_done=lambda: True, send_message=AsyncMock()),
        followup=followup,
        client=SimpleNamespace(player_service=SimpleNamespace(adjust_balance=lambda *a, **k: None)),
    )

    bot = SimpleNamespace(
        player_service=SimpleNamespace(get_player=lambda *a, **k: SimpleNamespace(discord_id=42)),
    )
    dig_service = SimpleNamespace(
        get_shop=lambda *a, **k: {
            "success": True, "error": None,
            "consumables": [{"name": "Dynamite", "price": 5, "description": "boom"}],
            "pickaxe_upgrades": [], "gear_for_sale": [], "inventory_count": 0,
        },
    )
    cog = DigCommands(bot, dig_service)

    await cog.dig_shop.callback(cog, interaction)

    # Deferred first (so it can't time out pre-defer).
    interaction.response.defer.assert_awaited()
    # The public attempt was made and rejected...
    assert followup.calls[0].get("ephemeral") in (False, None)
    # ...and the user still received the shop, privately.
    assert any(c.get("ephemeral") for c in followup.calls)


# ---------------------------------------------------------------------------
# Embed limit: the Consumables list must not blow Discord's 1024-char field cap
# ---------------------------------------------------------------------------

def test_shop_consumables_field_fits_discord_limit():
    """The Consumables list, rendered as the handler renders it, exceeds a single
    1024-char field — which 400'd `/dig shop` in prod (error 50035). It must split
    across fields with every consumable still shown.

    Reconstructs the handler's input faithfully: get_shop remaps each item's
    `cost` to `price` (progression_mixin.get_shop), so we build the same shape
    here — reading CONSUMABLE_ITEMS directly would leave every price as the '?'
    fallback and silently render a different string than prod.
    """
    consumables = [
        {"name": v["name"], "price": v["cost"], "description": v["description"]}
        for v in CONSUMABLE_ITEMS.values()
    ]
    lines = [
        f"**{c.get('name', '?')}** — {c.get('price', '?')} {JOPACOIN_EMOTE}: {c.get('description', '')}"
        for c in consumables
    ]
    # Every line shows its real price, not the '?' fallback (guards the cost->price
    # item shape the handler depends on).
    for c, line in zip(consumables, lines):
        assert f"— {c['price']} " in line
    # The bug condition: as one field this overflows Discord's limit.
    assert len("\n".join(lines)) > 1024

    embed = discord.Embed(title="Mining Shop")
    add_lines_field(embed, "Consumables", lines)

    # After splitting, the embed is valid to send...
    assert validate_embed(embed) == []
    # ...and no consumable was silently dropped.
    rendered = "\n".join(f.value for f in embed.fields)
    for c in consumables:
        assert c["name"] in rendered
