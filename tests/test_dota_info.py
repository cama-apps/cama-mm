"""Tests for DotaInfoCommands cog — hero/ability lookup and formatting helpers."""

from unittest.mock import AsyncMock, MagicMock, call, patch

import pytest

from commands.dota_info import (
    DotaInfoCommands,
    _format_ability_values,
    _get_ability_by_name,
    _get_all_abilities,
    _get_all_heroes,
    _get_hero_by_name,
)


async def _run_sync(func, *args, **kwargs):
    return func(*args, **kwargs)


@pytest.mark.asyncio
async def test_autocomplete_loaders_are_offloaded_from_the_event_loop():
    cog = DotaInfoCommands(MagicMock())
    with (
        patch(
            "commands.dota_info._get_all_heroes",
            return_value=[("Pudge", 14)],
        ) as get_heroes,
        patch(
            "commands.dota_info._get_all_abilities",
            return_value=[("Meat Hook", 14)],
        ) as get_abilities,
        patch(
            "commands.dota_info.asyncio.to_thread",
            new_callable=AsyncMock,
            side_effect=_run_sync,
        ) as to_thread,
    ):
        hero_choices = await cog.hero_autocomplete(MagicMock(), "pud")
        ability_choices = await cog.ability_autocomplete(MagicMock(), "hook")

    assert [choice.name for choice in hero_choices] == ["Pudge"]
    assert [choice.name for choice in ability_choices] == ["Meat Hook"]
    assert to_thread.await_args_list == [call(get_heroes), call(get_abilities)]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("command_name", "helper_name", "query", "not_found"),
    [
        ("hero", "_get_hero_by_name", "Pudge", "Hero 'Pudge' not found"),
        (
            "ability",
            "_get_ability_by_name",
            "Meat Hook",
            "Ability 'Meat Hook' not found",
        ),
    ],
)
async def test_detail_lookups_are_offloaded_from_the_event_loop(
    command_name,
    helper_name,
    query,
    not_found,
):
    cog = DotaInfoCommands(MagicMock())
    interaction = MagicMock()
    command = getattr(DotaInfoCommands, command_name)
    with (
        patch(f"commands.dota_info.{helper_name}", return_value=None) as lookup,
        patch(
            "commands.dota_info.asyncio.to_thread",
            new_callable=AsyncMock,
            side_effect=_run_sync,
        ) as to_thread,
        patch(
            "commands.dota_info.safe_defer",
            new_callable=AsyncMock,
            return_value=True,
        ),
        patch(
            "commands.dota_info.safe_followup", new_callable=AsyncMock
        ) as followup,
    ):
        await command.callback(cog, interaction, query)

    to_thread.assert_awaited_once_with(lookup, query)
    assert not_found in followup.await_args.kwargs["content"]


class TestDotaInfoHeroLookup:
    def test_get_all_heroes_shape(self):
        heroes = _get_all_heroes()
        assert isinstance(heroes, list)
        assert len(heroes) > 100
        assert all(isinstance(h, tuple) and len(h) == 2 for h in heroes)
        assert all(isinstance(h[0], str) and isinstance(h[1], int) for h in heroes)

    @pytest.mark.parametrize(
        "query,expected_name,expected_id",
        [
            ("Anti-Mage", "Anti-Mage", 1),
            ("anti-mage", "Anti-Mage", 1),
            ("am", "Anti-Mage", 1),
        ],
    )
    def test_hero_by_name_finds(self, query, expected_name, expected_id):
        hero = _get_hero_by_name(query)
        assert hero is not None
        assert hero.localized_name == expected_name
        assert hero.id == expected_id

    def test_hero_by_name_missing_returns_none(self):
        assert _get_hero_by_name("NotARealHero123") is None

    def test_hero_abilities_and_talents(self):
        pudge = _get_hero_by_name("Pudge")
        assert pudge is not None
        ability_names = [a.localized_name for a in pudge.abilities]
        assert "Meat Hook" in ability_names

        cm = _get_hero_by_name("Crystal Maiden")
        assert cm is not None
        assert len(cm.talents) >= 4


class TestAbilityLookup:
    def test_get_all_abilities_shape(self):
        abilities = _get_all_abilities()
        assert isinstance(abilities, list)
        assert len(abilities) > 200
        assert all(isinstance(a, tuple) and len(a) == 2 for a in abilities)

    @pytest.mark.parametrize("query", ["Meat Hook", "meat hook"])
    def test_ability_by_name_finds(self, query):
        ability = _get_ability_by_name(query)
        assert ability is not None
        assert ability.localized_name == "Meat Hook"

    def test_ability_by_name_missing_returns_none(self):
        assert _get_ability_by_name("NotARealAbility123") is None

    def test_ability_has_description(self):
        blink = _get_ability_by_name("Blink")
        assert blink is not None
        assert blink.description and len(blink.description) > 0


class TestFormatting:

    def test_format_ability_values_empty(self):
        class MockAbility:
            ability_special = None

        assert _format_ability_values(MockAbility()) == ""

    def test_format_ability_values_with_data(self):
        class MockAbility:
            ability_special = [
                {"header": "DAMAGE:", "value": "90 180 270 360"},
                {"header": "RANGE:", "value": "1100 1200 1300 1400"},
            ]

        result = _format_ability_values(MockAbility())
        assert "Damage" in result
        assert "90 180 270 360" in result


class TestHeroAttributes:
    @pytest.mark.parametrize(
        "name,attr",
        [
            ("Axe", "strength"),
            ("Phantom Assassin", "agility"),
            ("Crystal Maiden", "intelligence"),
        ],
    )
    def test_hero_primary_attr(self, name, attr):
        hero = _get_hero_by_name(name)
        assert hero is not None
        assert hero.attr_primary == attr

    def test_hero_base_stats_present(self):
        pudge = _get_hero_by_name("Pudge")
        assert pudge is not None
        assert pudge.attr_strength_base is not None
        assert pudge.attr_agility_base is not None
        assert pudge.attr_intelligence_base is not None
        assert pudge.base_movement is not None
        assert pudge.base_armor is not None

    def test_hero_has_roles(self):
        lion = _get_hero_by_name("Lion")
        assert lion is not None
        assert "Support" in lion.roles or "Disabler" in lion.roles


class TestHeroFacets:
    def test_hero_has_facets_with_descriptions(self):
        wd = _get_hero_by_name("Witch Doctor")
        assert wd is not None
        assert len(wd.facets) >= 2
        for facet in wd.facets:
            assert facet.localized_name
            assert facet.description
