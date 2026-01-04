"""
Dota 2 information commands using dotabase: /hero, /ability
"""

import logging
from functools import lru_cache

import discord
from discord import app_commands
from discord.ext import commands
from dotabase import Ability, Hero, dotabase_session

from utils.hero_lookup import get_hero_image_url
from utils.interaction_safety import safe_defer, safe_followup

logger = logging.getLogger("cama_bot.commands.dota_info")

# Attribute display names and colors
ATTR_DISPLAY = {
    "strength": ("STR", 0xE53935),
    "agility": ("AGI", 0x43A047),
    "intelligence": ("INT", 0x1E88E5),
    "universal": ("UNI", 0x8E24AA),
}


@lru_cache(maxsize=1)
def _get_all_heroes() -> list[tuple[str, int]]:
    """Get all hero names and IDs (cached)."""
    session = dotabase_session()
    heroes = session.query(Hero).all()
    return [(h.localized_name, h.id) for h in heroes]


@lru_cache(maxsize=1)
def _get_all_abilities() -> list[tuple[str, int]]:
    """Get all ability names and IDs (cached)."""
    session = dotabase_session()
    abilities = session.query(Ability).all()
    # Filter in Python since is_talent is a computed property
    # Deduplicate by name (some abilities appear multiple times)
    seen = set()
    result = []
    for a in abilities:
        if a.is_talent:
            continue
        name = a.localized_name
        if not name or name in seen:
            continue
        seen.add(name)
        result.append((name, a.id))
    return result


def _get_hero_by_name(name: str) -> Hero | None:
    """Find a hero by localized name (case-insensitive)."""
    session = dotabase_session()
    hero = session.query(Hero).filter(Hero.localized_name.ilike(name)).first()
    if hero:
        return hero
    # Try alias search
    for h in session.query(Hero).all():
        if h.aliases and name.lower() in h.aliases.lower():
            return h
    return None


def _get_ability_by_name(name: str) -> Ability | None:
    """Find an ability by localized name (case-insensitive)."""
    session = dotabase_session()
    # Filter by name first via SQL, then check is_talent in Python
    abilities = session.query(Ability).filter(Ability.localized_name.ilike(name)).all()
    for a in abilities:
        if not a.is_talent:
            return a
    return None


def _format_stat(label: str, value, suffix: str = "") -> str:
    """Format a stat line."""
    if value is None:
        return ""
    return f"**{label}:** {value}{suffix}"


def _format_ability_values(ability: Ability) -> str:
    """Format ability special values (scalable values by level)."""
    if not ability.ability_special:
        return ""

    lines = []
    import json

    try:
        specials = (
            json.loads(ability.ability_special)
            if isinstance(ability.ability_special, str)
            else ability.ability_special
        )
        for spec in specials[:6]:  # Limit to 6 values
            header = spec.get("header", spec.get("key", ""))
            value = spec.get("value", "")
            if header and value:
                # Clean up header
                header = header.replace("_", " ").title().strip()
                if header.endswith(":"):
                    header = header[:-1]
                lines.append(f"• {header}: {value}")
    except (json.JSONDecodeError, TypeError):
        pass

    return "\n".join(lines)


class DotaInfoCommands(commands.Cog):
    """Commands for Dota 2 hero and ability information."""

    def __init__(self, bot: commands.Bot):
        self.bot = bot

    async def hero_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for hero names."""
        heroes = _get_all_heroes()
        matches = [
            app_commands.Choice(name=name, value=name)
            for name, _ in heroes
            if current.lower() in name.lower()
        ]
        return matches[:25]  # Discord limit

    async def ability_autocomplete(
        self, interaction: discord.Interaction, current: str
    ) -> list[app_commands.Choice[str]]:
        """Autocomplete for ability names."""
        abilities = _get_all_abilities()
        matches = [
            app_commands.Choice(name=name, value=name)
            for name, _ in abilities
            if current.lower() in name.lower()
        ]
        return matches[:25]  # Discord limit

    @app_commands.command(name="hero", description="Get information about a Dota 2 hero")
    @app_commands.describe(hero_name="The hero name (e.g., Anti-Mage, Pudge)")
    @app_commands.autocomplete(hero_name=hero_autocomplete)
    async def hero(self, interaction: discord.Interaction, hero_name: str):
        """Display detailed hero information."""
        if not await safe_defer(interaction):
            return

        hero = _get_hero_by_name(hero_name)
        if not hero:
            await safe_followup(
                interaction,
                content=f"Hero '{hero_name}' not found. Try using the autocomplete suggestions.",
            )
            return

        # Determine primary attribute
        attr = hero.attr_primary or "universal"
        attr_name, attr_color = ATTR_DISPLAY.get(attr, ("UNI", 0x8E24AA))

        # Build embed
        embed = discord.Embed(
            title=f"{hero.localized_name}",
            description=hero.hype[:200] + "..."
            if hero.hype and len(hero.hype) > 200
            else hero.hype,
            color=attr_color,
        )

        # Add hero image
        hero_img = get_hero_image_url(hero.id)
        if hero_img:
            embed.set_thumbnail(url=hero_img)

        # Base stats
        stats_lines = []
        if hero.attr_strength_base:
            stats_lines.append(f"**STR:** {hero.attr_strength_base} (+{hero.attr_strength_gain})")
        if hero.attr_agility_base:
            stats_lines.append(f"**AGI:** {hero.attr_agility_base} (+{hero.attr_agility_gain})")
        if hero.attr_intelligence_base:
            stats_lines.append(
                f"**INT:** {hero.attr_intelligence_base} (+{hero.attr_intelligence_gain})"
            )
        if stats_lines:
            embed.add_field(
                name=f"Attributes ({attr_name})", value="\n".join(stats_lines), inline=True
            )

        # Combat stats
        combat_lines = []
        if hero.attack_damage_min and hero.attack_damage_max:
            combat_lines.append(f"**Damage:** {hero.attack_damage_min}-{hero.attack_damage_max}")
        if hero.attack_range:
            attack_type = "Melee" if hero.is_melee else "Ranged"
            combat_lines.append(f"**Range:** {hero.attack_range} ({attack_type})")
        if hero.attack_rate:
            combat_lines.append(f"**Attack Time:** {hero.attack_rate}")
        if hero.base_movement:
            combat_lines.append(f"**Move Speed:** {hero.base_movement}")
        if combat_lines:
            embed.add_field(name="Combat", value="\n".join(combat_lines), inline=True)

        # Defense
        defense_lines = []
        if hero.base_armor is not None:
            defense_lines.append(f"**Armor:** {hero.base_armor}")
        if hero.magic_resistance:
            defense_lines.append(f"**Magic Resist:** {hero.magic_resistance}%")
        if defense_lines:
            embed.add_field(name="Defense", value="\n".join(defense_lines), inline=True)

        # Roles
        if hero.roles:
            embed.add_field(name="Roles", value=hero.roles.replace("|", ", "), inline=False)

        # Abilities
        if hero.abilities:
            ability_list = []
            for ability in hero.abilities[:6]:  # Limit to 6
                name = ability.localized_name or "Unknown"
                ability_list.append(f"• {name}")
            if ability_list:
                embed.add_field(name="Abilities", value="\n".join(ability_list), inline=True)

        # Talents
        if hero.talents:
            talent_list = []
            for talent in hero.talents[:8]:  # Talents are usually 8
                name = talent.localized_name or "Unknown"
                talent_list.append(f"• {name}")
            if talent_list:
                embed.add_field(name="Talents", value="\n".join(talent_list), inline=True)

        # Facets
        if hero.facets:
            facet_list = []
            for facet in hero.facets:
                name = facet.localized_name or "Unknown"
                desc = (
                    facet.description[:60] + "..."
                    if facet.description and len(facet.description) > 60
                    else facet.description or ""
                )
                facet_list.append(f"**{name}**: {desc}")
            if facet_list:
                embed.add_field(name="Facets", value="\n".join(facet_list), inline=False)

        # Links
        hero_slug = hero.name.replace("npc_dota_hero_", "").replace("_", "-")
        embed.add_field(
            name="Links",
            value=f"[Dotabuff](https://www.dotabuff.com/heroes/{hero_slug}) | "
            f"[OpenDota](https://www.opendota.com/heroes/{hero.id})",
            inline=False,
        )

        await safe_followup(interaction, embed=embed)

    @app_commands.command(name="ability", description="Get information about a Dota 2 ability")
    @app_commands.describe(ability_name="The ability name (e.g., Blink, Mana Break)")
    @app_commands.autocomplete(ability_name=ability_autocomplete)
    async def ability(self, interaction: discord.Interaction, ability_name: str):
        """Display detailed ability information."""
        if not await safe_defer(interaction):
            return

        ability = _get_ability_by_name(ability_name)
        if not ability:
            await safe_followup(
                interaction,
                content=f"Ability '{ability_name}' not found. Try using the autocomplete suggestions.",
            )
            return

        # Build embed
        embed = discord.Embed(
            title=ability.localized_name,
            description=ability.description or "No description available.",
            color=0x7289DA,
        )

        # Ability properties
        props = []
        if ability.behavior:
            behavior = ability.behavior.replace("|", ", ").replace("_", " ").title()
            props.append(f"**Behavior:** {behavior}")
        if ability.damage_type:
            props.append(f"**Damage Type:** {ability.damage_type.title()}")
        if props:
            embed.add_field(name="Properties", value="\n".join(props), inline=False)

        # Values
        values = _format_ability_values(ability)
        if values:
            embed.add_field(name="Values", value=values, inline=False)

        # Upgrades
        upgrades = []
        if ability.scepter_upgrades and ability.scepter_description:
            desc = (
                ability.scepter_description[:100] + "..."
                if len(ability.scepter_description) > 100
                else ability.scepter_description
            )
            upgrades.append(f"**Scepter:** {desc}")
        if ability.shard_upgrades and ability.shard_description:
            desc = (
                ability.shard_description[:100] + "..."
                if len(ability.shard_description) > 100
                else ability.shard_description
            )
            upgrades.append(f"**Shard:** {desc}")
        if upgrades:
            embed.add_field(name="Upgrades", value="\n".join(upgrades), inline=False)

        # Lore
        if ability.lore:
            lore = ability.lore[:150] + "..." if len(ability.lore) > 150 else ability.lore
            embed.set_footer(text=lore)

        await safe_followup(interaction, embed=embed)


async def setup(bot: commands.Bot):
    """Setup function for loading cog dynamically."""
    await bot.add_cog(DotaInfoCommands(bot))
