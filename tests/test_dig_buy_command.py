"""The `/dig buy` autocomplete stays aligned with live shop data."""

from types import SimpleNamespace

import pytest

from commands.dig import DigCommands
from repositories.dig_repository import DigRepository
from repositories.player_repository import PlayerRepository
from services.dig_service import DigService


def _service(repo_db_path):
    dig_repo = DigRepository(repo_db_path)
    player_repo = PlayerRepository(repo_db_path)
    player_repo.add(discord_id=111, discord_username="pf", guild_id=0)
    player_repo.add_balance(111, 0, 10_000)
    dig_repo.create_tunnel(111, 0, "T")
    dig_repo.update_tunnel(
        111,
        0,
        depth=0,
        max_depth=275,
        prestige_level=5,
    )
    return DigService(dig_repo, player_repo)


async def _choices(service, query: str):
    command = SimpleNamespace(dig_service=service)
    interaction = SimpleNamespace(
        guild=SimpleNamespace(id=0),
        user=SimpleNamespace(id=111),
    )
    return await DigCommands.buy_autocomplete(command, interaction, query)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("whetstone", {"tempered_whetstone"}),
        ("warding", {"warding_salts"}),
        ("rescue", {"rescue_line"}),
        ("amulet", {f"amulet:{tier}" for tier in range(4, 8)}),
        ("boots", {f"boots:{tier}" for tier in range(4, 8)}),
    ],
)
async def test_buy_autocomplete_exposes_new_sinks(
    repo_db_path,
    query,
    expected,
):
    choices = await _choices(_service(repo_db_path), query)
    values = {choice.value for choice in choices}

    assert expected <= values
    assert all(choice.name.strip() for choice in choices)


@pytest.mark.asyncio
async def test_every_shop_row_is_reachable_through_filtered_autocomplete(repo_db_path):
    service = _service(repo_db_path)
    shop = service.get_shop(111, 0)

    for item in shop["consumables"]:
        choices = await _choices(service, item["name"])
        assert item["id"] in {choice.value for choice in choices}
    for item in shop["pickaxe_upgrades"]:
        choices = await _choices(service, item["name"])
        assert f"weapon:{item['tier']}" in {choice.value for choice in choices}
    for item in shop["gear_for_sale"]:
        choices = await _choices(service, item["name"])
        assert f"{item['slot']}:{item['tier']}" in {
            choice.value for choice in choices
        }
