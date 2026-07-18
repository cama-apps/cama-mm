"""Embed builders for the ``/dig artifacts`` catalog."""

from __future__ import annotations

import discord

from services.dig_constants import ALL_ARTIFACTS, format_relic_label
from services.dig_data.artifacts import ArtifactDef
from utils.embed_safety import EMBED_LIMITS, truncate_field

_FIELDS_PER_EMBED = 20
_FIELD_CHARACTER_BUDGET = EMBED_LIMITS["total"] - 700


def _definition_field(
    artifact: ArtifactDef,
    *,
    copies: int | None = None,
    equipped: int = 0,
) -> tuple[str, str]:
    """Format one catalog definition as a Discord-safe embed field."""
    ownership = ""
    if copies is not None:
        ownership = f" — Owned ×{copies}"
        if equipped:
            ownership += f" • Equipped ×{equipped}"

    field_name = truncate_field(
        f"{artifact.name}{ownership}", EMBED_LIMITS["field_name"]
    )
    artifact_type = "Relic" if artifact.is_relic else "Curio"
    metadata = f"**{artifact.rarity}** • **{artifact.layer}** • **{artifact_type}**"
    if artifact.min_prestige:
        metadata += f" • **Prestige P{artifact.min_prestige}+**"
    effect = artifact.effect or "Collectible only (no equip effect)."
    field_value = truncate_field(
        f"{metadata}\n*{artifact.lore_text}*\n**Effect:** {effect}",
        EMBED_LIMITS["field_value"],
    )
    return field_name, field_value


def _fallback_field(
    artifact_id: str,
    *,
    copies: int,
    equipped: int,
    is_relic: bool,
) -> tuple[str, str]:
    """Describe a generated or retired owned artifact absent from the catalog."""
    label = format_relic_label(artifact_id, with_stats=True)
    ownership = f" — Owned ×{copies}"
    if equipped:
        ownership += f" • Equipped ×{equipped}"
    field_name = truncate_field(
        f"{label}{ownership}", EMBED_LIMITS["field_name"]
    )

    if artifact_id.startswith("pinnacle:"):
        rarity = "Unique"
        layer = "Special reward"
        lore = "A one-of-a-kind artifact whose properties were rolled when it was found."
        effect = "Its generated effects are listed in the artifact name."
    else:
        rarity = "Legacy"
        layer = "Unknown layer"
        lore = "This owned artifact is no longer present in the current catalog."
        effect = "No current effect description is available."
    artifact_type = "Relic" if is_relic else "Curio"
    field_value = truncate_field(
        (
            f"**{rarity}** • **{layer}** • **{artifact_type}**\n"
            f"*{lore}*\n**Effect:** {effect}"
        ),
        EMBED_LIMITS["field_value"],
    )
    return field_name, field_value


def _paginate_fields(
    fields: list[tuple[str, str]],
    *,
    title: str,
    description: str,
    footer_label: str,
    empty_message: str | None = None,
) -> list[discord.Embed]:
    """Build pages below Discord's field-count and aggregate text limits."""
    field_pages: list[list[tuple[str, str]]] = []
    current: list[tuple[str, str]] = []
    current_characters = 0

    for field in fields:
        field_characters = len(field[0]) + len(field[1])
        if current and (
            len(current) >= _FIELDS_PER_EMBED
            or current_characters + field_characters > _FIELD_CHARACTER_BUDGET
        ):
            field_pages.append(current)
            current = []
            current_characters = 0
        current.append(field)
        current_characters += field_characters

    if current:
        field_pages.append(current)
    elif not field_pages:
        field_pages.append([])

    page_count = len(field_pages)
    embeds: list[discord.Embed] = []
    for page_number, page_fields in enumerate(field_pages, start=1):
        page_title = title
        if page_count > 1:
            page_title += f" ({page_number}/{page_count})"
        embed = discord.Embed(
            title=truncate_field(page_title, EMBED_LIMITS["title"]),
            description=description if page_number == 1 else None,
            color=0xD4AF37,
        )
        if not page_fields and empty_message:
            embed.description = f"{description}\n\n{empty_message}"
        for field_name, field_value in page_fields:
            embed.add_field(name=field_name, value=field_value, inline=False)
        embed.set_footer(text=f"{footer_label} • Page {page_number}/{page_count}")
        if (
            len(embed.fields) > EMBED_LIMITS["max_fields"]
            or len(embed) > EMBED_LIMITS["total"]
        ):
            raise ValueError("Artifact catalog page exceeds Discord embed limits")
        embeds.append(embed)
    return embeds


def build_artifact_catalog_embeds(owned_rows: list[dict]) -> list[discord.Embed]:
    """Build the full catalog followed by the invoking user's collection."""
    catalog_embeds = _paginate_fields(
        [_definition_field(artifact) for artifact in ALL_ARTIFACTS],
        title="Dig Artifact Catalog",
        description=(
            "Every discoverable artifact. Relics have equippable effects; "
            "curios are collectible keepsakes."
        ),
        footer_label=f"Catalog: {len(ALL_ARTIFACTS)} artifacts",
    )

    owned: dict[str, dict[str, int | bool]] = {}
    for raw_row in owned_rows or []:
        row = dict(raw_row)
        artifact_id = str(row.get("artifact_id") or "")
        if not artifact_id:
            continue
        summary = owned.setdefault(
            artifact_id,
            {"copies": 0, "equipped": 0, "is_relic": False},
        )
        summary["copies"] = int(summary["copies"]) + 1
        summary["equipped"] = int(summary["equipped"]) + int(
            bool(row.get("equipped"))
        )
        summary["is_relic"] = bool(summary["is_relic"] or row.get("is_relic"))

    owned_fields: list[tuple[str, str]] = []
    for artifact in ALL_ARTIFACTS:
        summary = owned.pop(artifact.id, None)
        if summary is not None:
            owned_fields.append(
                _definition_field(
                    artifact,
                    copies=int(summary["copies"]),
                    equipped=int(summary["equipped"]),
                )
            )
    for artifact_id, summary in sorted(owned.items()):
        owned_fields.append(
            _fallback_field(
                artifact_id,
                copies=int(summary["copies"]),
                equipped=int(summary["equipped"]),
                is_relic=bool(summary["is_relic"]),
            )
        )

    owned_embeds = _paginate_fields(
        owned_fields,
        title="Your Artifacts",
        description=(
            "Artifacts owned by you in this server, repeated from the catalog "
            "for quick reference."
        ),
        footer_label=f"Your collection: {len(owned_fields)} unique artifacts",
        empty_message="You haven't found any artifacts yet.",
    )
    return [*catalog_embeds, *owned_embeds]
