"""Views, modal, and embed builders for dig boss encounters and duels."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from types import SimpleNamespace
from typing import TYPE_CHECKING

import discord

from commands.dig_helpers._shared import _wrap
from commands.dig_helpers.route_views import (
    RouteChoiceView,
    add_route_choice_fields,
    get_route_choice,
)
from services.dig_constants import BOSS_WAGER_MAX_JC
from services.dig_constants import get_layer as get_layer_def
from utils.formatting import JOPACOIN_EMOTE
from utils.interaction_safety import safe_defer, safe_followup
from utils.neon_helpers import get_neon_service, send_neon_result

if TYPE_CHECKING:
    from services.dig_service import DigService

logger = logging.getLogger("cama_bot.commands.dig")

_ECHO_EFFECT_COPY = "-25% max HP and 30% less wager profit"

BossResolvedCallback = Callable[[int, int | None], Awaitable[None]]


def _add_gear_broken_notice(embed: discord.Embed, result) -> None:
    raw = result._d if hasattr(result, "_d") else (result if isinstance(result, dict) else {})
    gear_broken = getattr(result, "gear_broken", None) or raw.get("gear_broken") or []
    if not gear_broken:
        return
    embed.add_field(
        name="Gear Broken",
        value=(
            "\n".join(f"• **{name}**" for name in gear_broken)
            + "\nThese items stay equipped with their effects disabled until repaired. "
            "Use **Repair All** in `/dig gear`."
        ),
        inline=False,
    )


def _add_carried_wager_notice(embed: discord.Embed, boss_info) -> None:
    raw_amount = getattr(boss_info, "carried_wager", None)
    try:
        amount = int(raw_amount)
    except (TypeError, ValueError):
        return
    if amount <= 0:
        return
    embed.add_field(
        name="Carried Wager",
        value=(
            f"**{amount:,}** {JOPACOIN_EMOTE} is already riding on this phase."
        ),
        inline=False,
    )


async def _run_boss_resolved_callback(
    callback: BossResolvedCallback | None,
    user_id: int,
    guild_id: int | None,
) -> None:
    """Run the post-commit callback without disturbing result rendering."""
    if callback is None:
        return
    try:
        await callback(user_id, guild_id)
    except Exception:
        logger.warning(
            "Boss resolved callback failed for user %s in guild %s",
            user_id,
            guild_id,
            exc_info=True,
        )


async def _send_boss_victory_neon(interaction, *, result, user_id, guild_id) -> None:
    """Best-effort: post a rare neon GIF when a boss duel is won."""
    if not getattr(result, "won", False):
        return
    try:
        neon = get_neon_service(interaction.client)
        if not neon:
            return
        boundary = getattr(result, "boundary", None) or 0
        try:
            depth_for_layer = int(getattr(result, "new_depth", 0) or 0) or int(boundary or 0)
        except (TypeError, ValueError):
            depth_for_layer = 0
        ld = get_layer_def(depth_for_layer)
        ln = ld.name if ld else "Dirt"
        nr = await neon.on_dig_boss_victory(
            user_id,
            guild_id,
            boss_name=getattr(result, "boss_name", "the guardian"),
            boundary=int(boundary or 0),
            layer_name=ln,
            jc_delta=getattr(result, "payout", 0) or 0,
            gear_drop=getattr(result, "gear_drop", None),
            trophy_relic_drop=getattr(result, "trophy_relic_drop", None),
        )
        await send_neon_result(interaction, nr)
    except Exception:
        logger.debug("Boss victory neon failed", exc_info=True)


class BossWagerModal(discord.ui.Modal):
    """Modal for entering boss fight wager details."""

    risk_tier = discord.ui.TextInput(
        label="Risk Tier (cautious / bold / reckless)",
        placeholder="bold",
        min_length=1,
        max_length=10,
        required=True,
    )

    wager = discord.ui.TextInput(
        label=f"Wager Amount (max {BOSS_WAGER_MAX_JC:,} JC)",
        placeholder=f"0-{BOSS_WAGER_MAX_JC}",
        min_length=1,
        max_length=10,
        required=True,
    )

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        dig_flavor_service=None,
        on_boss_resolved: BossResolvedCallback | None = None,
    ):
        super().__init__(title="Boss Fight Wager")
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.dig_flavor_service = dig_flavor_service
        self.on_boss_resolved = on_boss_resolved
        self.result = None

    async def on_submit(self, interaction: discord.Interaction):
        tier = self.risk_tier.value.strip().lower()
        if tier not in ("cautious", "bold", "reckless"):
            await interaction.response.send_message(
                "Invalid risk tier. Choose: cautious, bold, or reckless.", ephemeral=True
            )
            return

        try:
            amount = int(self.wager.value)
        except ValueError:
            await interaction.response.send_message(
                "Invalid wager amount. Please enter a number.", ephemeral=True
            )
            return

        if amount < 0:
            await interaction.response.send_message(
                "Wager must be non-negative.", ephemeral=True
            )
            return

        await safe_defer(interaction, thinking=True)
        try:
            self.result = _wrap(await asyncio.to_thread(
                self.dig_service.start_boss_duel,
                self.user_id,
                self.guild_id,
                tier,
                amount,
            ))

            if not getattr(self.result, "success", True):
                error_msg = getattr(self.result, "error", "Boss fight failed.")
                embed = discord.Embed(
                    title="Boss Fight Error",
                    description=error_msg,
                    color=0xFFA500,
                )
                await interaction.followup.send(embed=embed, ephemeral=True)
                return

            # Mid-fight prompt: boss's rolled mechanic triggered at its round.
            # Hand off to the BossDuelView so the player picks one of 3
            # reactive options before the duel resumes.
            if getattr(self.result, "pending_prompt", None):
                view = BossDuelView(
                    dig_service=self.dig_service,
                    user_id=self.user_id,
                    guild_id=self.guild_id,
                    initial_result=self.result,
                    risk_tier=tier,
                    wager=amount,
                    dig_flavor_service=self.dig_flavor_service,
                    on_boss_resolved=self.on_boss_resolved,
                )
                embed = _build_duel_prompt_embed(self.result)
                msg = await interaction.followup.send(embed=embed, view=view, wait=True)
                view.message = msg
                self.stop()
                return

            await _run_boss_resolved_callback(
                self.on_boss_resolved, self.user_id, self.guild_id,
            )

            # Next phase incoming — survival flavor + auto-engage it.
            # The wager rides forward (carried on boss_progress), so the new
            # encounter view will skip the wager modal on Fight.
            if (
                getattr(self.result, "phase2_incoming", False)
                or getattr(self.result, "phase3_incoming", False)
            ):
                boss_name = getattr(self.result, "boss_name", "the boss")
                victory_embed = discord.Embed(
                    title="Phase Cleared",
                    description=f"You broke **{boss_name}** — it staggers...",
                    color=0x00FF00,
                )
                await interaction.followup.send(embed=victory_embed)
                await asyncio.sleep(2)
                channel = interaction.channel
                if channel is not None:
                    await _post_phase_transition_followup(
                        channel,
                        dig_service=self.dig_service,
                        user_id=self.user_id,
                        guild_id=self.guild_id,
                        result=self.result,
                        dig_flavor_service=self.dig_flavor_service,
                        on_boss_resolved=self.on_boss_resolved,
                    )
                return

            # LLM narrative for the boss fight (best-effort)
            boss_narrative = None
            result_dict = self.result._d if hasattr(self.result, "_d") else {}
            if self.dig_flavor_service and result_dict:
                try:
                    boss_narrative = await self.dig_flavor_service.narrate_boss_fight(
                        result_dict, self.user_id, self.guild_id,
                    )
                except Exception:
                    logger.debug("Boss fight narration failed", exc_info=True)

            embed = discord.Embed(
                title="Boss Fight Result",
                color=0x00FF00 if getattr(self.result, "won", False) else 0xFF0000,
            )
            boss_name = getattr(self.result, "boss_name", "the boss")
            win_chance = getattr(self.result, "win_chance", 0)
            if getattr(self.result, "won", False):
                payout = getattr(self.result, "payout", 0)
                embed.description = (
                    f"Victory! You defeated **{boss_name}** and won "
                    f"**{payout:+d}** {JOPACOIN_EMOTE} profit!"
                )
                penalty = getattr(self.result, "bankruptcy_penalty", 0) or 0
                if penalty > 0:
                    embed.description += (
                        f"\n−{penalty} {JOPACOIN_EMOTE} withheld while bankrupt."
                    )
                if boss_narrative:
                    embed.add_field(name="​", value=f"*{boss_narrative}*", inline=False)
                if getattr(self.result, "stat_point_awarded", False):
                    embed.add_field(
                        name="S Point Earned",
                        value="First clear bonus: use `/dig miner build` to allocate it.",
                        inline=False,
                    )
            else:
                loss = abs(getattr(self.result, "jc_delta", 0)) or amount
                knockback = getattr(self.result, "knockback", 0)
                embed.description = (
                    f"Defeat! **{boss_name}** overpowered you. "
                    f"You lost **{loss}** {JOPACOIN_EMOTE}"
                    f" and were knocked back {knockback} blocks."
                )
                if boss_narrative:
                    embed.add_field(name="​", value=f"*{boss_narrative}*", inline=False)
            _add_gear_broken_notice(embed, self.result)
            embed.add_field(
                name="Details",
                value=(
                    f"Risk: {tier.title()} | Pre-fight win chance: "
                    f"{int(win_chance * 100)}%"
                ),
                inline=False,
            )

            if getattr(self.result, "echo_applied", False):
                killer_id = getattr(self.result, "echo_killer_id", None)
                killer_mention = f"<@{killer_id}>" if killer_id else "a guildmate"
                embed.add_field(
                    name="Echoing in the Tunnels",
                    value=(
                        f"{killer_mention} killed this boss recently. "
                        f"It came in weakened ({_ECHO_EFFECT_COPY})."
                    ),
                    inline=False,
                )

            route_choice = get_route_choice(self.result)
            route_view = None
            if route_choice is not None:
                add_route_choice_fields(embed, route_choice)
                route_view = RouteChoiceView(
                    self.dig_service,
                    self.user_id,
                    self.guild_id,
                    route_choice,
                )

            # Try to load boss fight result art — prefer the locked boss_id,
            # fall back to the depth boundary for the grandfathered slug.
            boss_file = None
            boundary = getattr(self.result, "boundary", None)
            boss_id = getattr(self.result, "boss_id", "") or boundary
            won = getattr(self.result, "won", False)
            if boss_id:
                try:
                    from utils.dig_assets import get_boss_art
                    new_depth = getattr(self.result, "new_depth", 0)
                    ld = get_layer_def(new_depth or boundary)
                    ln = ld.name if ld else "Dirt"
                    scene = "victory" if won else "defeat"
                    boss_file = await asyncio.to_thread(get_boss_art, boss_id, scene, ln)
                except Exception as e:
                    logger.debug("Boss fight art failed: %s", e)

            if boss_file:
                embed.set_image(url=f"attachment://{boss_file.filename}")
                if route_view is not None:
                    msg = await interaction.followup.send(
                        embed=embed,
                        file=boss_file,
                        view=route_view,
                        wait=True,
                    )
                    route_view.message = msg
                else:
                    await interaction.followup.send(embed=embed, file=boss_file)
            else:
                if route_view is not None:
                    msg = await interaction.followup.send(
                        embed=embed,
                        view=route_view,
                        wait=True,
                    )
                    route_view.message = msg
                else:
                    await interaction.followup.send(embed=embed)

            await _send_boss_victory_neon(
                interaction, result=self.result, user_id=self.user_id, guild_id=self.guild_id
            )
        except Exception as e:
            logger.error("Boss fight error: %s", e, exc_info=True)
            await interaction.followup.send("Boss fight failed. Try again.", ephemeral=True)


class BossRiskModal(discord.ui.Modal):
    """Modal for choosing a non-final boss phase risk tier without a wager."""

    risk_tier = discord.ui.TextInput(
        label="Risk Tier (cautious / bold / reckless)",
        placeholder="bold",
        min_length=1,
        max_length=10,
        required=True,
    )

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        dig_flavor_service=None,
        on_boss_resolved: BossResolvedCallback | None = None,
    ):
        super().__init__(title="Boss Phase Risk")
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.dig_flavor_service = dig_flavor_service
        self.on_boss_resolved = on_boss_resolved

    async def on_submit(self, interaction: discord.Interaction):
        tier = self.risk_tier.value.strip().lower()
        if tier not in ("cautious", "bold", "reckless"):
            await interaction.response.send_message(
                "Invalid risk tier. Choose: cautious, bold, or reckless.", ephemeral=True
            )
            return

        await safe_defer(interaction, thinking=True)
        try:
            await _resolve_phase_fight_without_modal(
                interaction,
                dig_service=self.dig_service,
                user_id=self.user_id,
                guild_id=self.guild_id,
                risk_tier=tier,
                wager=0,
                dig_flavor_service=self.dig_flavor_service,
                on_boss_resolved=self.on_boss_resolved,
            )
        except Exception as e:
            logger.error("Boss phase risk fight error: %s", e, exc_info=True)
            try:
                await safe_followup(
                    interaction,
                    content="Boss fight failed. Try again.",
                    ephemeral=True,
                )
            except Exception:
                logger.warning("Boss phase risk failure followup failed", exc_info=True)
        finally:
            self.stop()


async def _post_phase_transition_followup(
    channel,
    *,
    dig_service: DigService,
    user_id: int,
    guild_id: int | None,
    result,
    dig_flavor_service=None,
    on_boss_resolved: BossResolvedCallback | None = None,
) -> None:
    """Post the transformation flavor embed and auto-engage the next phase.

    Posts directly to the supplied ``channel`` (typically
    ``interaction.channel`` or ``view.message.channel``) so the helper can
    be invoked from both interaction and view contexts uniformly.
    """
    next_phase_title = getattr(result, "next_phase_title", "")
    phase2_name = getattr(result, "phase2_name", "") or next_phase_title or "???"
    phase2_title = getattr(result, "phase2_title", "") or next_phase_title
    p2_dialogue = getattr(result, "dialogue", "...")
    boss_name = getattr(result, "boss_name", "the boss")
    wager = getattr(result, "wager", 0)

    transition_embed = discord.Embed(
        title=f"{phase2_title or phase2_name} Emerges!",
        description=(
            f"**{boss_name}** is transforming!\n\n"
            f"**{phase2_name}**\n"
            f"*{p2_dialogue}*"
        ),
        color=0x8B0000,
    )
    if wager:
        transition_embed.add_field(
            name="​",
            value=f"Your **{wager}** {JOPACOIN_EMOTE} wager rides on the next phase.",
            inline=False,
        )
    _add_gear_broken_notice(transition_embed, result)

    # Phase-3 transitions (from the resume_boss_duel path) reach this helper
    # with phase3_incoming=True; pick the right asset key so the embed art
    # matches the phase the player is actually about to fight.
    phase_key = "phase3" if getattr(result, "phase3_incoming", False) else "phase2"
    p2_file = None
    boundary = getattr(result, "boundary", None)
    boss_id = getattr(result, "boss_id", "") or boundary
    if boss_id:
        try:
            from utils.dig_assets import get_boss_art
            new_depth = getattr(result, "new_depth", 0)
            ld = get_layer_def(new_depth or boundary)
            ln = ld.name if ld else "Dirt"
            p2_file = await asyncio.to_thread(get_boss_art, boss_id, phase_key, ln)
        except Exception:
            p2_file = None

    if p2_file:
        transition_embed.set_image(url=f"attachment://{p2_file.filename}")
        await channel.send(embed=transition_embed, file=p2_file)
    else:
        await channel.send(embed=transition_embed)

    # Refetch the encounter for the next phase and post a fresh
    # BossEncounterView. The Fight button will see the carry and skip the
    # wager modal.
    try:
        info_dict = await asyncio.to_thread(
            dig_service.build_next_boss_encounter, user_id, guild_id,
        )
    except Exception:
        logger.debug("Phase auto-continue: build_next_boss_encounter failed", exc_info=True)
        info_dict = None
    if not info_dict:
        # Service couldn't build the next encounter — surface a recovery
        # hint so the player isn't left staring at a dead-end embed.
        await channel.send(
            content="The next phase didn't load — use `/dig go` to engage it.",
        )
        return

    next_info = SimpleNamespace(**info_dict)
    encounter_embed = discord.Embed(
        title=f"Boss Encountered: {getattr(next_info, 'name', 'Unknown Boss')}!",
        description=getattr(next_info, "dialogue", ""),
        color=0xFF0000,
    )
    _add_carried_wager_notice(encounter_embed, next_info)
    lum_line = getattr(next_info, "luminosity_display", None)
    if lum_line:
        encounter_embed.add_field(name="​", value=lum_line, inline=False)
    has_lantern = await asyncio.to_thread(
        dig_service.has_scout_lantern, user_id, guild_id,
    )
    view = BossEncounterView(
        dig_service, user_id, guild_id, next_info,
        has_lantern,
        dig_flavor_service=dig_flavor_service,
        on_boss_resolved=on_boss_resolved,
    )
    msg = await channel.send(embed=encounter_embed, view=view)
    if msg:
        view.message = msg


async def _resolve_phase_fight_without_modal(
    interaction: discord.Interaction,
    *,
    dig_service: DigService,
    user_id: int,
    guild_id: int | None,
    risk_tier: str,
    wager: int,
    dig_flavor_service=None,
    on_boss_resolved: BossResolvedCallback | None = None,
) -> None:
    """Engage a multi-phase fight using the carried wager (no modal).

    Mirrors the modal's on_submit result handling so phase 2/3 fights flow
    through the same mechanic-prompt / transformation / final-result paths.
    """
    try:
        result = _wrap(await asyncio.to_thread(
            dig_service.start_boss_duel, user_id, guild_id, risk_tier, wager,
        ))
    except Exception as e:
        logger.error("Phase fight error: %s", e, exc_info=True)
        await safe_followup(interaction, content="Boss fight failed.", ephemeral=True)
        return

    if not getattr(result, "success", True):
        await safe_followup(
            interaction,
            content=getattr(result, "error", "Boss fight failed."),
            ephemeral=True,
        )
        return

    if getattr(result, "pending_prompt", None):
        view = BossDuelView(
            dig_service=dig_service,
            user_id=user_id,
            guild_id=guild_id,
            initial_result=result,
            risk_tier=risk_tier,
            wager=wager,
            dig_flavor_service=dig_flavor_service,
            on_boss_resolved=on_boss_resolved,
        )
        embed = _build_duel_prompt_embed(result)
        msg = await interaction.followup.send(embed=embed, view=view, wait=True)
        view.message = msg
        return

    await _run_boss_resolved_callback(on_boss_resolved, user_id, guild_id)

    if getattr(result, "phase2_incoming", False) or getattr(result, "phase3_incoming", False):
        channel = interaction.channel
        if channel is not None:
            await _post_phase_transition_followup(
                channel,
                dig_service=dig_service, user_id=user_id, guild_id=guild_id,
                result=result, dig_flavor_service=dig_flavor_service,
                on_boss_resolved=on_boss_resolved,
            )
        return

    embed = discord.Embed(
        title="Boss Fight Result",
        color=0x00FF00 if getattr(result, "won", False) else 0xFF0000,
    )
    boss_name = getattr(result, "boss_name", "the boss")
    win_chance = getattr(result, "win_chance", 0)
    if getattr(result, "won", False):
        payout = getattr(result, "payout", 0)
        embed.description = (
            f"Victory! You defeated **{boss_name}** and won "
            f"**{payout:+d}** {JOPACOIN_EMOTE} profit!"
        )
        penalty = getattr(result, "bankruptcy_penalty", 0) or 0
        if penalty > 0:
            embed.description += (
                f"\n−{penalty} {JOPACOIN_EMOTE} withheld while bankrupt."
            )
    else:
        loss = abs(getattr(result, "jc_delta", 0))
        knockback = getattr(result, "knockback", 0)
        embed.description = (
            f"Defeat! **{boss_name}** overpowered you. "
            f"You lost **{loss}** {JOPACOIN_EMOTE} and were knocked back "
            f"{knockback} blocks."
        )
    soften_line = getattr(result, "soften_line", None)
    if soften_line:
        embed.add_field(name="​", value=soften_line, inline=False)
    _add_gear_broken_notice(embed, result)
    embed.add_field(
        name="Details",
        value=(
            f"Risk: {risk_tier.title()} | Pre-fight win chance: "
            f"{int(win_chance * 100)}%"
        ),
        inline=False,
    )
    route_choice = get_route_choice(result)
    if route_choice is not None:
        add_route_choice_fields(embed, route_choice)
        route_view = RouteChoiceView(
            dig_service,
            user_id,
            guild_id,
            route_choice,
        )
        msg = await interaction.followup.send(
            embed=embed,
            view=route_view,
            wait=True,
        )
        route_view.message = msg
    else:
        await interaction.followup.send(embed=embed)
    await _send_boss_victory_neon(
        interaction, result=result, user_id=user_id, guild_id=guild_id
    )


def _build_duel_prompt_embed(result) -> discord.Embed:
    """Embed rendered alongside a BossDuelView's three reactive option buttons.

    Accepts either the raw dict returned by ``start_boss_duel`` /
    ``resume_boss_duel`` or a ``_wrap``'d object.
    """
    raw = result._d if hasattr(result, "_d") else (result if isinstance(result, dict) else {})
    pp = raw.get("pending_prompt") or {}
    boss_name = raw.get("boss_name") or "the boss"
    player_hp = raw.get("player_hp", 0)
    player_hp_max = raw.get("player_hp_max", player_hp)
    boss_hp = raw.get("boss_hp", 0)
    boss_hp_max = raw.get("boss_hp_max", boss_hp)
    round_num = raw.get("round_num", 0)
    is_pinnacle = bool(raw.get("is_pinnacle"))
    phase = raw.get("phase")
    phase_total = raw.get("phase_total")

    title = pp.get("prompt_title") or f"{boss_name} acts"
    description = pp.get("prompt_description") or ""

    header_parts = [boss_name, f"Round {round_num}"]
    if is_pinnacle and phase and phase_total:
        header_parts.insert(1, f"Phase {phase}/{phase_total}")
    header = " — ".join(header_parts)

    embed = discord.Embed(
        title=header,
        description=f"**{title}**\n*{description}*",
        color=0xB22222 if is_pinnacle else 0xFFD700,
    )
    embed.add_field(
        name="State",
        value=(
            f"You: **{player_hp}/{player_hp_max}** HP  |  "
            f"{boss_name}: **{boss_hp}/{boss_hp_max}** HP"
        ),
        inline=False,
    )
    lum_line = raw.get("luminosity_display")
    if lum_line:
        embed.add_field(name="​", value=lum_line, inline=False)
    _add_gear_broken_notice(embed, result)
    opts = pp.get("options") or []
    if opts:
        lines = [f"**{o['option_idx'] + 1}.** {o['label']}" for o in opts]
        embed.add_field(
            name="Your choice",
            value=(
                "\n".join(lines)
                + "\n\n*(120s before the safe option is auto-picked.)*"
            ),
            inline=False,
        )
    return embed


async def _load_boss_result_art(result) -> discord.File | None:
    """Resolve the victory/defeat art for a boss-fight result, or None.

    Mirrors the boss_id → boundary fallback used elsewhere so grandfathered
    bosses without a locked id still resolve their slug.
    """
    boundary = getattr(result, "boundary", None)
    boss_id = getattr(result, "boss_id", "") or boundary
    if not boss_id:
        return None
    try:
        from utils.dig_assets import get_boss_art
        # boss_progress JSON keys are stored as strings; coerce so
        # get_layer_def's int comparison doesn't blow up on a "25"-shaped
        # boundary and silently fall back to no art.
        try:
            depth_for_layer = (
                int(getattr(result, "new_depth", 0) or 0)
                or int(boundary or 0)
            )
        except (TypeError, ValueError):
            depth_for_layer = 0
        ld = get_layer_def(depth_for_layer)
        ln = ld.name if ld else "Dirt"
        scene = "victory" if getattr(result, "won", False) else "defeat"
        return await asyncio.to_thread(get_boss_art, boss_id, scene, ln)
    except Exception as e:
        logger.debug("Boss fight art failed: %s", e)
        return None


def _build_boss_fight_result_embed(*, result, risk_tier: str, amount: int) -> discord.Embed:
    """Shared post-duel result embed — used by the modal's no-prompt path and
    by ``BossDuelView`` after the final option click / timeout."""
    won = getattr(result, "won", False)
    boss_name = getattr(result, "boss_name", "the boss")
    win_chance = getattr(result, "win_chance", 0) or 0
    embed = discord.Embed(
        title="Boss Fight Result",
        color=0x00FF00 if won else 0xFF0000,
    )
    if won:
        payout = getattr(result, "payout", 0)
        embed.description = (
            f"Victory! You defeated **{boss_name}** and won "
            f"**{payout:+d}** {JOPACOIN_EMOTE} profit!"
        )
        penalty = getattr(result, "bankruptcy_penalty", 0) or 0
        if penalty > 0:
            embed.description += (
                f"\n−{penalty} {JOPACOIN_EMOTE} withheld while bankrupt."
            )
        if getattr(result, "stat_point_awarded", False):
            embed.add_field(
                name="S Point Earned",
                value="First clear bonus: use `/dig miner build` to allocate it.",
                inline=False,
            )
        gear_drop = getattr(result, "gear_drop", None)
        if gear_drop:
            gd = gear_drop if isinstance(gear_drop, dict) else (
                gear_drop._d if hasattr(gear_drop, "_d") else None
            )
            if gd:
                embed.add_field(
                    name="Boss Drop",
                    value=f"**{gd.get('name', 'Gear')}** ({gd.get('slot', 'gear')})",
                    inline=False,
                )
        relic_drop = getattr(result, "prestige_relic_drop", None)
        if relic_drop:
            rd = relic_drop if isinstance(relic_drop, dict) else (
                relic_drop._d if hasattr(relic_drop, "_d") else None
            )
            if rd:
                embed.add_field(
                    name="Relic Found",
                    value=f"**{rd.get('name', 'Relic')}**",
                    inline=False,
                )
        trophy_drop = getattr(result, "trophy_relic_drop", None)
        if trophy_drop:
            td = trophy_drop if isinstance(trophy_drop, dict) else (
                trophy_drop._d if hasattr(trophy_drop, "_d") else None
            )
            if td:
                embed.add_field(
                    name="Trophy Carved",
                    value=f"**{td.get('name', 'Trophy')}**",
                    inline=False,
                )
    else:
        loss = abs(getattr(result, "jc_delta", 0)) or amount
        knockback = getattr(result, "knockback", 0)
        embed.description = (
            f"Defeat! **{boss_name}** overpowered you. "
            f"You lost **{loss}** {JOPACOIN_EMOTE} and were knocked back {knockback} blocks."
        )

    raw = result._d if hasattr(result, "_d") else (result if isinstance(result, dict) else {})
    _add_gear_broken_notice(embed, result)

    # Surface the mid-fight option the player picked plus its rolled narrative.
    # Without this, picking a button on the reactive prompt jumps straight to
    # a generic "Defeat!" and the player has no idea what happened.
    round_log = raw.get("round_log") or []
    mechanic_entry = next(
        (e for e in reversed(round_log) if isinstance(e, dict) and e.get("mechanic_id")),
        None,
    )
    if mechanic_entry:
        option_label = mechanic_entry.get("option_label") or "Your choice"
        narrative = mechanic_entry.get("narrative") or ""
        if narrative:
            embed.add_field(
                name=f"You chose: {option_label}",
                value=narrative,
                inline=False,
            )
        extra_kb = getattr(result, "extra_knockback", 0)
        extra_cd = getattr(result, "extra_cooldown_s", 0)
        if extra_kb or extra_cd:
            parts = []
            if extra_kb:
                parts.append(f"+{extra_kb} extra knockback")
            if extra_cd:
                parts.append(f"+{extra_cd // 60}m extra cooldown")
            embed.add_field(
                name="Loss Penalty", value="; ".join(parts), inline=False,
            )
    soften_line = getattr(result, "soften_line", None)
    if soften_line:
        embed.add_field(name="​", value=soften_line, inline=False)
    embed.add_field(
        name="Details",
        value=(
            f"Risk: {risk_tier.title()} | Pre-fight win chance: "
            f"{int(win_chance * 100)}%"
        ),
        inline=False,
    )
    lum_line = getattr(result, "luminosity_display", None)
    if lum_line:
        embed.add_field(name="​", value=lum_line, inline=False)
    if getattr(result, "echo_applied", False):
        killer_id = getattr(result, "echo_killer_id", None)
        killer_mention = f"<@{killer_id}>" if killer_id else "a guildmate"
        embed.add_field(
            name="Echoing in the Tunnels",
            value=(
                f"{killer_mention} killed this boss recently. "
                f"It came in weakened ({_ECHO_EFFECT_COPY})."
            ),
            inline=False,
        )
    # Pinnacle-specific surfaces: phase transition or relic drop.
    if getattr(result, "is_pinnacle", False):
        if getattr(result, "phase2_incoming", False) or getattr(result, "phase3_incoming", False):
            next_title = getattr(result, "next_phase_title", None)
            event_flavor = getattr(result, "phase_event_flavor", "")
            event_desc = getattr(result, "phase_event_description", "")
            value_lines = []
            if next_title:
                value_lines.append(f"Next: **{next_title}**")
            if event_flavor:
                value_lines.append(f"*{event_flavor}*")
            if event_desc:
                value_lines.append(event_desc)
            if value_lines:
                embed.add_field(
                    name="Phase shift",
                    value="\n".join(value_lines),
                    inline=False,
                )
        if getattr(result, "pinnacle_defeated", False):
            relic = getattr(result, "pinnacle_relic", None) or {}
            if relic:
                embed.add_field(
                    name=f"Relic: {relic.get('name', '?')}",
                    value="\n".join(f"• {s}" for s in (relic.get("stats") or [])) or "—",
                    inline=False,
                )
    route_choice = get_route_choice(result)
    if route_choice is not None:
        add_route_choice_fields(embed, route_choice)
    return embed


class BossDuelView(discord.ui.View):
    """Interactive view for a paused boss duel.

    Rendered whenever ``start_boss_duel`` or ``resume_boss_duel`` returns a
    ``pending_prompt`` — the initial prompt and any continuation prompts both
    re-instantiate this view via ``_render_resolution``. Creates one button
    per option in the mechanic's three-reactive-option prompt. Click rolls the
    option's distribution and resumes the duel. Timeout (120s) auto-picks the
    mechanic's designated safe option.
    """

    def __init__(
        self,
        *,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        initial_result,
        risk_tier: str,
        wager: int,
        dig_flavor_service=None,
        on_boss_resolved: BossResolvedCallback | None = None,
    ):
        super().__init__(timeout=120)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.risk_tier = risk_tier
        self.wager = wager
        self.dig_flavor_service = dig_flavor_service
        self.on_boss_resolved = on_boss_resolved
        self.message: discord.Message | None = None
        self._resolved = False

        raw = (
            initial_result._d if hasattr(initial_result, "_d")
            else (initial_result if isinstance(initial_result, dict) else {})
        )
        pp = raw.get("pending_prompt") or {}
        self._safe_option_idx = int(pp.get("safe_option_idx", 0))
        for opt in pp.get("options", []):
            idx = int(opt.get("option_idx", 0))
            label = (opt.get("label") or f"Option {idx + 1}")[:80]
            btn = discord.ui.Button(
                label=label,
                style=discord.ButtonStyle.primary,
                custom_id=f"duel_opt_{idx}",
            )
            btn.callback = self._make_callback(idx)
            self.add_item(btn)

    def _make_callback(self, option_idx: int):
        async def callback(interaction: discord.Interaction):
            if interaction.user.id != self.user_id:
                await interaction.response.send_message(
                    "Only the duelist can choose.", ephemeral=True,
                )
                return
            await safe_defer(interaction)
            await self._submit(option_idx, interaction)
        return callback

    async def on_timeout(self):
        if self._resolved:
            return
        await self._submit(self._safe_option_idx)

    async def _submit(self, option_idx: int, interaction: discord.Interaction | None = None) -> None:
        if self._resolved:
            return
        self._resolved = True
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.resume_boss_duel,
                self.user_id, self.guild_id, option_idx,
            ))
        except Exception as e:
            logger.error("Boss duel resume failed: %s", e, exc_info=True)
            await self._edit_message(content="Duel resume failed.", embed=None, view=None)
            return
        if not getattr(result, "success", True):
            err = getattr(result, "error", "Duel resume failed.")
            await self._edit_message(content=err, embed=None, view=None)
            return
        if not getattr(result, "pending_prompt", None):
            await _run_boss_resolved_callback(
                self.on_boss_resolved, self.user_id, self.guild_id,
            )
        # Service-side state is already mutated by resume_boss_duel — if the
        # render path raises, the user otherwise sees frozen buttons with no
        # confirmation that their rewards/loss landed. Surface a fallback
        # instead of swallowing.
        try:
            await self._render_resolution(result)
        except Exception as e:
            logger.error("Boss duel render failed: %s", e, exc_info=True)
            await self._edit_message(
                content="Fight resolved but display failed — check `/dig info` for your current state.",
                embed=None, view=None,
            )

        # Only the final outcome is a true victory. _render_resolution handles the
        # re-prompt and phase-transition branches (which carry won=True mid-fight),
        # so guard against firing a premature victory GIF on a non-final phase.
        is_final = not (
            getattr(result, "pending_prompt", None)
            or getattr(result, "phase2_incoming", False)
            or getattr(result, "phase3_incoming", False)
        )
        if interaction is not None and is_final:
            await _send_boss_victory_neon(
                interaction, result=result, user_id=self.user_id, guild_id=self.guild_id
            )

    async def _render_resolution(self, result) -> None:
        """Render either a follow-up prompt or the final fight outcome."""
        if getattr(result, "pending_prompt", None):
            new_view = BossDuelView(
                dig_service=self.dig_service,
                user_id=self.user_id, guild_id=self.guild_id,
                initial_result=result,
                risk_tier=self.risk_tier, wager=self.wager,
                dig_flavor_service=self.dig_flavor_service,
                on_boss_resolved=self.on_boss_resolved,
            )
            embed = _build_duel_prompt_embed(result)
            await self._edit_message(embed=embed, view=new_view)
            new_view.message = self.message
            self.stop()
            return

        # Multi-phase transition through a mid-fight prompt: clean up the
        # active duel message and post the auto-continue follow-up.
        if (
            getattr(result, "phase2_incoming", False)
            or getattr(result, "phase3_incoming", False)
        ):
            cleared_embed = discord.Embed(
                title="Phase Cleared",
                description=(
                    f"You broke **{getattr(result, 'boss_name', 'the boss')}** — "
                    "it staggers..."
                ),
                color=0x00FF00,
            )
            await self._edit_message(embed=cleared_embed, view=None)
            channel = getattr(self.message, "channel", None) if self.message else None
            if channel is not None:
                await _post_phase_transition_followup(
                    channel,
                    dig_service=self.dig_service,
                    user_id=self.user_id, guild_id=self.guild_id,
                    result=result,
                    dig_flavor_service=self.dig_flavor_service,
                    on_boss_resolved=self.on_boss_resolved,
                )
            self.stop()
            return

        embed = _build_boss_fight_result_embed(
            result=result, risk_tier=self.risk_tier, amount=self.wager,
        )
        route_choice = get_route_choice(result)
        route_view = (
            RouteChoiceView(
                self.dig_service,
                self.user_id,
                self.guild_id,
                route_choice,
            )
            if route_choice is not None
            else None
        )
        boss_file = await _load_boss_result_art(result)
        if boss_file:
            embed.set_image(url=f"attachment://{boss_file.filename}")
            await self._edit_message(
                embed=embed,
                view=route_view,
                attachments=[boss_file],
            )
        else:
            await self._edit_message(embed=embed, view=route_view)
        if route_view is not None:
            route_view.message = self.message
        self.stop()

    async def _edit_message(self, **kwargs) -> None:
        """Edit the duel's original message, falling back to channel.send().

        The timeout path fires after Discord's interaction token is already
        expired (>15 min), at which point ``message.edit`` raises
        ``discord.HTTPException``. Without the fallback the user sees stale
        buttons forever. Logged at WARNING so recurring failures are visible.
        """
        if self.message is None:
            return
        try:
            await self.message.edit(**kwargs)
            return
        except Exception as e:
            logger.warning("BossDuelView message edit failed: %s", e)
        channel = getattr(self.message, "channel", None)
        if channel is None:
            return
        embed = kwargs.get("embed")
        content = kwargs.get("content")
        attachments = kwargs.get("attachments") or []
        files = [a for a in attachments if isinstance(a, discord.File)]
        try:
            if embed is not None:
                if files:
                    await channel.send(embed=embed, files=files)
                else:
                    await channel.send(embed=embed)
            elif content:
                await channel.send(content=content)
        except Exception as e:
            logger.warning("BossDuelView channel fallback also failed: %s", e)


class BossEncounterView(discord.ui.View):
    """View for boss encounter interactions."""

    def __init__(
        self,
        dig_service: DigService,
        user_id: int,
        guild_id: int | None,
        boss_info: object,
        has_lantern: bool = False,
        dig_flavor_service=None,
        on_boss_resolved: BossResolvedCallback | None = None,
    ):
        super().__init__(timeout=60)
        self.dig_service = dig_service
        self.user_id = user_id
        self.guild_id = guild_id
        self.boss_info = boss_info
        self.has_lantern = has_lantern
        self.dig_flavor_service = dig_flavor_service
        self.on_boss_resolved = on_boss_resolved
        self.message: discord.Message | None = None
        # Guards against a fast double-click on Fight re-entering resolution
        # before the first click's await completes and stops the view.
        self._engaged = False
        if not has_lantern:
            self.scout.disabled = True

    async def on_timeout(self):
        for child in self.children:
            if isinstance(child, discord.ui.Button):
                child.disabled = True
        if self.message is None:
            return
        try:
            await self.message.edit(view=self)
        except (discord.NotFound, discord.HTTPException) as exc:
            logger.warning("Boss encounter timeout edit failed: %s", exc)

    @discord.ui.button(label="Fight", style=discord.ButtonStyle.danger, emoji="\u2694\ufe0f")
    async def fight(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            # Others can cheer, not fight
            await interaction.response.send_message("Only the tunnel owner can fight.", ephemeral=True)
            return
        if self._engaged:
            # A prior click already started resolution; swallow the duplicate.
            await safe_defer(interaction)
            return
        self._engaged = True
        if getattr(self.boss_info, "wager_allowed", True) is False:
            modal = BossRiskModal(
                self.dig_service,
                self.user_id,
                self.guild_id,
                dig_flavor_service=self.dig_flavor_service,
                on_boss_resolved=self.on_boss_resolved,
            )
            await interaction.response.send_modal(modal)
            self.stop()
            return
        # Multi-phase carry: a prior phase win locked the original wager onto
        # this boss. Skip the wager modal — the carried stake rides forward.
        carried = await asyncio.to_thread(
            self.dig_service.get_carried_wager, self.user_id, self.guild_id,
        )
        if carried:
            await safe_defer(interaction)
            await _resolve_phase_fight_without_modal(
                interaction,
                dig_service=self.dig_service,
                user_id=self.user_id,
                guild_id=self.guild_id,
                risk_tier=carried["risk_tier"],
                wager=int(carried["wager"]),
                dig_flavor_service=self.dig_flavor_service,
                on_boss_resolved=self.on_boss_resolved,
            )
            self.stop()
            return
        modal = BossWagerModal(
            self.dig_service,
            self.user_id,
            self.guild_id,
            dig_flavor_service=self.dig_flavor_service,
            on_boss_resolved=self.on_boss_resolved,
        )
        await interaction.response.send_modal(modal)
        self.stop()

    @discord.ui.button(label="Retreat", style=discord.ButtonStyle.secondary, emoji="\U0001f3c3")
    async def retreat(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Only the tunnel owner can retreat.", ephemeral=True)
            return
        await safe_defer(interaction)
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.retreat_boss, self.user_id, self.guild_id
            ))
            if not getattr(result, "success", True):
                await safe_followup(
                    interaction,
                    content=getattr(result, "error", "Retreat failed."),
                    ephemeral=True,
                )
            else:
                loss = getattr(result, "loss", 0)
                new_depth = getattr(result, "new_depth", 0)
                await safe_followup(
                    interaction,
                    content=f"You retreated safely, losing {loss} blocks. Now at depth {new_depth}.",
                )
        except Exception as e:
            logger.error("Boss retreat error: %s", e, exc_info=True)
            await safe_followup(interaction, content="Retreat failed.", ephemeral=True)
        self.stop()

    @discord.ui.button(label="Scout", style=discord.ButtonStyle.primary, emoji="\U0001f526")
    async def scout(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Only the tunnel owner can scout.", ephemeral=True)
            return
        await safe_defer(interaction)
        try:
            info = _wrap(await asyncio.to_thread(
                self.dig_service.scout_boss, self.user_id, self.guild_id
            ))
            if not getattr(info, "success", True):
                await safe_followup(
                    interaction,
                    content=getattr(info, "error", "Scouting failed."),
                    ephemeral=True,
                )
                return
            boss_name = getattr(info, "boss_name", "Unknown Boss")
            odds = getattr(info, "odds", None)
            if odds and hasattr(odds, "_d"):
                odds = odds._d
            lines = [f"**{boss_name}** — Intel Report\n"]
            if getattr(info, "echo_applied", False):
                killer_id = getattr(info, "echo_killer_id", None)
                killer_mention = f"<@{killer_id}>" if killer_id else "a guildmate"
                lines.append(
                    f"*Weakened — {killer_mention} killed this boss in the last 24h. "
                    f"({_ECHO_EFFECT_COPY})*\n"
                )
            if isinstance(odds, dict):
                for tier in ("cautious", "bold", "reckless"):
                    t = odds.get(tier)
                    if not t:
                        continue
                    win = int(t.get("win_pct", 0) * 100)
                    free = int(t.get("free_fight_pct", 0) * 100)
                    mult = t.get("multiplier", 1)
                    lines.append(
                        f"**{tier.title()}** — {win}% win"
                        f" ({free}% free) | {mult}x payout"
                    )
            else:
                lines.append("Could not read odds data.")

            # Great Lantern tier: additionally show the mechanic pool + stinger
            # warning so the player can plan counters and inventory.
            enhanced = getattr(info, "enhanced", False)
            mech_pool = getattr(info, "mechanic_pool", None)
            stinger = getattr(info, "stinger", None)
            if enhanced and (mech_pool or stinger):
                lines.append("\n_Great Lantern reveal_")
                if mech_pool:
                    lines.append("**Possible mid-fight mechanics** (one rolls per fight):")
                    for m in mech_pool:
                        lines.append(f"  • _{m.get('prompt_title', m.get('id', ''))}_")
                if stinger:
                    kb = stinger.get("extra_knockback", 0)
                    cd = stinger.get("extended_cooldown_s", 0)
                    curse = stinger.get("cursed_status")
                    bits = []
                    if kb:
                        bits.append(f"+{kb} extra knockback")
                    if cd:
                        bits.append(f"+{cd // 60}m extra cooldown")
                    if curse:
                        bits.append(f"curse: `{curse}`")
                    tail = f" ({'; '.join(bits)})" if bits else ""
                    lines.append(
                        "**On-loss stinger:** "
                        f"_{stinger.get('flavor_on_loss', '')}_" + tail
                    )
            embed = discord.Embed(
                title="Boss Scouted" + (" (Great Lantern)" if enhanced else ""),
                description="\n".join(lines),
                color=0xFFD700,
            )
            await safe_followup(interaction, embed=embed, ephemeral=True)
        except Exception as e:
            logger.error("Boss scout error: %s", e, exc_info=True)
            await safe_followup(interaction, content="Scouting failed.", ephemeral=True)

    @discord.ui.button(label="Cheer", style=discord.ButtonStyle.success, emoji="\U0001f4e3")
    async def cheer(self, interaction: discord.Interaction, button: discord.ui.Button):
        if interaction.user.id == self.user_id:
            await interaction.response.send_message("You can't cheer for yourself!", ephemeral=True)
            return
        await safe_defer(interaction)
        try:
            result = _wrap(await asyncio.to_thread(
                self.dig_service.cheer_boss,
                interaction.user.id,
                self.user_id,
                self.guild_id,
            ))
            if not getattr(result, "success", True):
                error_msg = getattr(result, "error", "Cheer failed.")
                await safe_followup(interaction, content=error_msg, ephemeral=True)
                return
            boost_pct = int(getattr(result, "total_boost", 0) * 100)
            cheer_count = getattr(result, "cheer_count", 0)
            await safe_followup(
                interaction,
                content=(
                    f"{interaction.user.display_name} cheers for the fighter! "
                    f"Boss odds boosted by +{boost_pct}% ({cheer_count}/3 cheers)"
                ),
            )
        except Exception as e:
            logger.error("Boss cheer error: %s", e, exc_info=True)
            await safe_followup(interaction, content="Cheer failed.", ephemeral=True)
