"""Command-level coverage for the ``/dig artifacts`` catalog."""

import re
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest

import commands.dig as dig_commands
from services.dig_data.artifacts import ALL_ARTIFACTS
from utils.embed_safety import EMBED_LIMITS, validate_embed

USER_ID = 10_001
GUILD_ID = 12_345


def _interaction() -> SimpleNamespace:
    return SimpleNamespace(
        guild=SimpleNamespace(id=GUILD_ID),
        user=SimpleNamespace(id=USER_ID, display_name="Catalog Reader"),
        channel=SimpleNamespace(id=999),
        response=SimpleNamespace(send_message=AsyncMock()),
        followup=SimpleNamespace(send=AsyncMock()),
    )


def _embed_text(embed) -> str:
    """Flatten user-visible embed text while preserving message order."""
    parts = [embed.title or "", embed.description or ""]
    for field in embed.fields:
        parts.extend((field.name, field.value))
    if embed.footer and embed.footer.text:
        parts.append(embed.footer.text)
    return "\n".join(parts)


def _assert_discord_safe(interaction, embeds) -> None:
    assert len(embeds) == interaction.followup.send.await_count
    for call, embed in zip(
        interaction.followup.send.await_args_list,
        embeds,
        strict=True,
    ):
        content = call.kwargs.get("content") or ""
        assert len(content) <= 2_000
        assert validate_embed(embed) == []
        assert len(embed) <= EMBED_LIMITS["total"]


async def _invoke_artifacts_command(monkeypatch, owned_rows: list[dict]):
    monkeypatch.setattr(
        dig_commands,
        "require_dig_channel",
        AsyncMock(return_value=True),
    )
    monkeypatch.setattr(
        dig_commands,
        "_check_registered",
        AsyncMock(return_value=object()),
    )
    monkeypatch.setattr(
        dig_commands,
        "safe_defer",
        AsyncMock(return_value=True),
    )

    get_artifacts_for_catalog = Mock(return_value=owned_rows)
    dig_service = SimpleNamespace(
        get_artifacts_for_catalog=get_artifacts_for_catalog,
    )
    bot = SimpleNamespace(
        player_service=SimpleNamespace(get_player=Mock(return_value=object())),
    )
    interaction = _interaction()
    cog = dig_commands.DigCommands(bot, dig_service)

    await cog.dig_artifacts.callback(cog, interaction)

    embeds = [
        call.kwargs["embed"]
        for call in interaction.followup.send.await_args_list
        if call.kwargs.get("embed") is not None
    ]
    return interaction, get_artifacts_for_catalog, embeds


def test_artifacts_is_a_dig_slash_subcommand():
    command = dig_commands.DigCommands.dig_artifacts

    assert command.name == "artifacts"
    assert "artifact" in command.description.lower()


@pytest.mark.asyncio
async def test_artifacts_describes_catalog_and_repeats_owned_at_bottom(monkeypatch):
    owned_ids = {"mole_claws", "map_of_the_first_descent"}
    owned_rows = [
        {
            "id": index,
            "artifact_id": artifact_id,
            "is_relic": artifact_id != "map_of_the_first_descent",
            "equipped": False,
        }
        for index, artifact_id in enumerate(sorted(owned_ids), start=1)
    ]

    _, _, embeds = await _invoke_artifacts_command(monkeypatch, owned_rows)

    assert embeds
    rendered_pages = [_embed_text(embed) for embed in embeds]
    rendered = "\n".join(rendered_pages)

    # The main catalog must describe every static artifact, including curios.
    for artifact in ALL_ARTIFACTS:
        assert artifact.name in rendered
        assert artifact.layer in rendered
        assert artifact.rarity in rendered
        assert artifact.lore_text in rendered
        if artifact.effect:
            assert artifact.effect in rendered

    # Owned definitions are repeated in the final section; unowned definitions
    # appear only in the complete catalog above it.
    for artifact in ALL_ARTIFACTS:
        expected_count = 2 if artifact.id in owned_ids else 1
        assert rendered.count(artifact.name) == expected_count

    owned_section_pages = [
        index
        for index, page in enumerate(rendered_pages)
        if "Your Artifacts" in page
    ]
    assert owned_section_pages == [len(embeds) - 1]
    for artifact_id in owned_ids:
        artifact = next(item for item in ALL_ARTIFACTS if item.id == artifact_id)
        assert artifact.name in rendered_pages[-1]
        assert artifact.lore_text in rendered_pages[-1]


@pytest.mark.asyncio
async def test_artifacts_scopes_ownership_and_paginates_safely(monkeypatch):
    owned_rows = [
        {
            "id": 1,
            "artifact_id": "echo_stone",
            "is_relic": True,
            "equipped": True,
        },
    ]

    interaction, get_artifacts, embeds = await _invoke_artifacts_command(
        monkeypatch,
        owned_rows,
    )

    get_artifacts.assert_called_once_with(USER_ID, GUILD_ID)
    assert len(embeds) > 1
    owned_text = "\n".join(
        _embed_text(embed)
        for embed in embeds
        if "Your Artifacts" in (embed.title or "")
    )
    assert "Equipped ×1" in owned_text
    _assert_discord_safe(interaction, embeds)


@pytest.mark.asyncio
async def test_artifacts_paginates_full_owned_collection_after_catalog(monkeypatch):
    owned_rows = [
        {
            "id": index,
            "artifact_id": artifact.id,
            "is_relic": artifact.is_relic,
            "equipped": False,
        }
        for index, artifact in enumerate(ALL_ARTIFACTS, start=1)
    ]

    interaction, _, embeds = await _invoke_artifacts_command(monkeypatch, owned_rows)

    rendered_pages = [_embed_text(embed) for embed in embeds]
    catalog_pages = [
        index
        for index, page in enumerate(rendered_pages)
        if "Artifact Catalog" in page
    ]
    owned_pages = [
        index
        for index, page in enumerate(rendered_pages)
        if "Your Artifacts" in page
    ]
    assert catalog_pages
    assert len(owned_pages) > 1
    assert max(catalog_pages) < min(owned_pages)

    catalog_text = "\n".join(rendered_pages[: min(owned_pages)])
    owned_text = "\n".join(rendered_pages[min(owned_pages) :])
    for artifact in ALL_ARTIFACTS:
        assert artifact.name in catalog_text
        assert artifact.lore_text in catalog_text
        assert artifact.name in owned_text
        assert artifact.lore_text in owned_text
        if artifact.effect:
            assert artifact.effect in owned_text

    _assert_discord_safe(interaction, embeds)


@pytest.mark.asyncio
async def test_artifacts_handles_duplicate_owned_rows(monkeypatch):
    owned_rows = [
        {
            "id": row_id,
            "artifact_id": "mole_claws",
            "is_relic": True,
            "equipped": False,
        }
        for row_id in (1, 2)
    ]

    interaction, _, embeds = await _invoke_artifacts_command(monkeypatch, owned_rows)

    rendered_pages = [_embed_text(embed) for embed in embeds]
    first_owned_page = next(
        index
        for index, page in enumerate(rendered_pages)
        if "Your Artifacts" in page
    )
    owned_text = "\n".join(rendered_pages[first_owned_page:])
    assert owned_text.count("Mole Claws") == 1
    assert re.search(r"(?:[x×]\s*2|2\s*[x×])", owned_text, re.IGNORECASE)
    _assert_discord_safe(interaction, embeds)


@pytest.mark.asyncio
async def test_artifacts_has_bottom_section_for_empty_collection(monkeypatch):
    interaction, get_artifacts, embeds = await _invoke_artifacts_command(monkeypatch, [])

    get_artifacts.assert_called_once_with(USER_ID, GUILD_ID)
    assert "Your Artifacts" in _embed_text(embeds[-1])
    owned_text = _embed_text(embeds[-1]).lower()
    assert any(
        phrase in owned_text
        for phrase in (
            "haven't found",
            "not discovered",
            "no artifacts",
            "none yet",
        )
    )
    _assert_discord_safe(interaction, embeds)
