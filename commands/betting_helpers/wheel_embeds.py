"""Embed builders for the Wheel of Fortune (`/gamba`) result and explosion screens."""

from __future__ import annotations

import discord

from commands.betting_helpers.messages import WHEEL_EXPLOSION_REWARD
from utils.economy_scaling import scale_minigame_jc_delta
from utils.formatting import JOPACOIN_EMOTE


def wedge_ev(wedge: tuple) -> float:
    """Return a rough EV for a bankrupt wheel wedge (used to find 'worst' option)."""
    from utils.wheel_drawing import _SPECIAL_WEDGE_EST_EVS, _load_special_wedge_evs
    _load_special_wedge_evs()
    _, v, _ = wedge
    if isinstance(v, int):
        return float(v)
    return _SPECIAL_WEDGE_EST_EVS.get(v, 0.0)


def build_wheel_result_embed(
    result: tuple,
    new_balance: int,
    garnished: int,
    next_spin_time: int,
    shell_victim: discord.Member | None = None,
    shell_victim_new_balance: int | None = None,
    shell_amount: int = 0,
    shell_self_hit: bool = False,
    shell_missed: bool = False,
    lightning_total: int = 0,
    lightning_count: int = 0,
    lightning_victims: list | None = None,
    extend_games_added: int = 0,
    extend_new_total: int = 0,
    is_bankrupt: bool = False,
    is_golden: bool = False,
    jailbreak_new_total: int = 0,
    chain_value: int | None = None,
    chain_username: str = "someone",
    emergency_count: int = 0,
    emergency_total: int = 0,
    commune_total: int = 0,
    commune_count: int = 0,
    pardon_consumed: bool = False,
    heist_total: int = 0,
    heist_count: int = 0,
    market_crash_total: int = 0,
    market_crash_count: int = 0,
    compound_amount: int = 0,
    trickle_total: int = 0,
    trickle_count: int = 0,
    dividend_amount: int = 0,
    takeover_amount: int = 0,
    takeover_victim_name: str = "rank #4",
    takeover_missed: bool = False,
    recession_total: int = 0,
    recession_count: int = 0,
    recession_self_loss: int = 0,
    banana_victim: discord.Member | None = None,
    banana_victim_name: str = "the player behind you",
    banana_victim_loss: int = 0,
    banana_missed: bool = False,
    green_shell_victim: discord.Member | None = None,
    green_shell_victim_name: str = "someone",
    green_shell_amount: int = 0,
    green_shell_missed: bool = False,
    bomb_omb_victims: list | None = None,
    bomb_omb_burn_total: int = 0,
    bomb_omb_missed: bool = False,
    shield_absorbed_total: int = 0,
    shielded_count: int = 0,
    bankruptcy_penalty: int = 0,
) -> discord.Embed:
    """Build the final result embed after the wheel stops."""
    label, value = result[0], result[1]  # (label, value, color)

    if value == "JAILBREAK":
        title = "🔓 JAILBREAK! 🔓"
        color = discord.Color.from_str("#0a2a0a")
        description = (
            f"**JAIL**\n\n"
            f"You found a crack in the cell wall.\n\n"
            f"**−1 penalty game** removed!\n\n"
            f"Penalty games remaining: **{jailbreak_new_total}**\n\n"
            f"*Don't celebrate yet. You're still in here.*"
        )

    elif value == "CHAIN_REACTION":
        title = "⛓️ CHAIN REACTION! ⛓️"
        color = discord.Color.from_str("#1a1a3a")
        if chain_value is None:
            description = (
                "**CHAIN**\n\n"
                "⛓️ The chain reaches back... but finds nothing.\n\n"
                "*No prior normal wheel spin found. Fallback: nothing happens.*"
            )
        elif chain_value > 0:
            description = (
                f"**CHAIN**\n\n"
                f"⛓️ You copied **{chain_username}**'s last spin: **+{chain_value} JC**!\n\n"
                f"*Their luck became yours.*"
            )
        elif chain_value < 0:
            description = (
                f"**CHAIN**\n\n"
                f"⛓️ You copied **{chain_username}**'s last spin: **{chain_value} JC**.\n\n"
                f"*Their misfortune became yours. Tragic.*"
            )
        else:
            description = (
                f"**CHAIN**\n\n"
                f"⛓️ You copied **{chain_username}**'s last spin: **nothing happened**.\n\n"
                f"*The chain found only silence.*"
            )

    elif value == "EMERGENCY":
        title = "🚨 EMERGENCY! 🚨"
        color = discord.Color.from_str("#2a1a00")
        emergency_loss_cap = scale_minigame_jc_delta(20)
        description = (
            f"**SOS**\n\n"
            f"🚨 Economic crisis triggered!\n\n"
            f"**{emergency_count}** players each lost up to "
            f"**{emergency_loss_cap}** {JOPACOIN_EMOTE}.\n"
            f"Total drained: **{emergency_total}** {JOPACOIN_EMOTE} (vanished).\n\n"
            f"*No one is safe. Not even you.*"
        )

    elif value == "COMMUNE":
        title = "🫳 SEIZE THE MEANS! 🫳"
        color = discord.Color.from_str("#1a2a1a")
        description = (
            f"**SEIZE**\n\n"
            f"{commune_count} players each donated 1 JC. "
            f"You received **+{commune_total} JC** from the collective.\n\n"
            f"*From each according to their balance, to you.*"
        )

    elif value == "COMEBACK":
        title = "🃏 CLUTCH SAVE! 🃏"
        color = discord.Color.from_str("#0a1a2a")
        description = (
            "**CLUTCH**\n\n"
            "Fortune smiles — once. Your next BANKRUPT will be converted to a LOSE instead. "
            "*Don't waste it.*"
        )

    elif value in ("EXTEND_1", "EXTEND_2"):
        # Bankruptcy penalty extension (only appears on bankrupt wheel)
        color = discord.Color.dark_red()
        if extend_games_added == 0:
            title = "⛓️ Nothing to Extend"
            description = (
                f"**{label}**\n\n"
                f"Your debt is punishment enough. No penalty games to extend.\n\n"
                f"*Pay off your debts.*"
            )
        else:
            title = "⛓️ PENALTY EXTENDED! ⛓️"
            description = (
                f"**{label} GAME{'S' if extend_games_added > 1 else ''}**\n\n"
                f"Your bankruptcy penalty has been extended by **{extend_games_added}** game{'s' if extend_games_added > 1 else ''}!\n\n"
                f"New penalty games remaining: **{extend_new_total}**\n\n"
                f"*The wheel remembers your sins... keep winning to escape!*"
            )

    elif value == "RED_SHELL":
        # Mario Kart Red Shell outcome
        if shell_missed:
            title = "🔴 RED SHELL MISSED! 🔴"
            color = discord.Color.dark_gray()
            description = (
                f"**{label}**\n\n"
                f"The Red Shell circles the track but finds no eligible target!\n\n"
                f"*There's no eligible player ahead to hit.*"
            )
        else:
            title = "🔴 RED SHELL HIT! 🔴"
            color = discord.Color.red()
            victim_name = shell_victim.mention if shell_victim else "the player above"
            description = (
                f"**{label}**\n\n"
                f"💥 Red Shell locked onto {victim_name}!\n"
                f"You stole **{shell_amount}** {JOPACOIN_EMOTE}!\n\n"
                f"*Victim's new balance: **{shell_victim_new_balance}** {JOPACOIN_EMOTE}*"
            )

    elif value == "BLUE_SHELL":
        # Mario Kart Blue Shell outcome
        if shell_missed:
            # Edge case: no players in leaderboard (shouldn't happen in practice)
            title = "🔵 BLUE SHELL MISSED! 🔵"
            color = discord.Color.dark_gray()
            description = (
                f"**{label}**\n\n"
                f"The Blue Shell circles the track but finds no target!\n\n"
                f"*There's no one to hit...*"
            )
        elif shell_self_hit:
            title = "🔵 BLUE SHELL... SELF-HIT! 🔵"
            color = discord.Color.dark_blue()
            description = (
                f"**{label}**\n\n"
                f"💥 The Blue Shell targets the leader... **THAT'S YOU!**\n"
                f"You lost **{shell_amount}** {JOPACOIN_EMOTE}!\n\n"
                f"*The price of being on top... maybe diversify next time.*"
            )
        else:
            title = "🔵 BLUE SHELL STRIKE! 🔵"
            color = discord.Color.blue()
            victim_name = shell_victim.mention if shell_victim else "the richest player"
            description = (
                f"**{label}**\n\n"
                f"💥 Blue Shell targets the leader: {victim_name}!\n"
                f"You stole **{shell_amount}** {JOPACOIN_EMOTE}!\n\n"
                f"*Victim's new balance: **{shell_victim_new_balance}** {JOPACOIN_EMOTE}*"
            )

    elif value == "LIGHTNING_BOLT":
        title = "⚡ LIGHTNING BOLT! ⚡"
        color = discord.Color.from_str("#f39c12")
        victim_lines = ""
        if lightning_victims:
            for vname, vamt, _ in lightning_victims[:3]:
                victim_lines += f"⚡ **{vname}** lost **{vamt}** JC\n"
        description = (
            f"**{label}**\n\n"
            f"Lightning strikes the entire server!\n\n"
            f"**{lightning_count}** players hit for a total of **{lightning_total}** {JOPACOIN_EMOTE}\n"
            f"All funds sent to the Jopacoin Reserve.\n\n"
            f"{victim_lines}\n"
            f"*No one is safe.*"
        )

    # --- Golden Wheel mechanics ---
    elif value == "HEIST":
        title = "🥇 HEIST! 🥇"
        color = discord.Color.from_str("#7a5c00")
        if heist_count == 0:
            description = (
                f"**HEIST**\n\n"
                f"You cased the joint... but the bottom 30 are already broke.\n\n"
                f"*Consolation prize: **+{heist_total}** {JOPACOIN_EMOTE}.*"
            )
        else:
            description = (
                f"**HEIST**\n\n"
                f"💰 You robbed **{heist_count}** players at the bottom of the ladder!\n"
                f"Total stolen: **{heist_total}** {JOPACOIN_EMOTE}\n\n"
                f"*Crime pays — when you're already on top.*"
            )

    elif value == "MARKET_CRASH":
        title = "📉 MARKET CRASH! 📉"
        color = discord.Color.from_str("#8a4000")
        if market_crash_count == 0:
            description = (
                f"**CRASH**\n\n"
                f"You triggered a crash... but you're the only whale. No one to tax.\n\n"
                f"*Consolation prize: **+{market_crash_total}** {JOPACOIN_EMOTE}.*"
            )
        else:
            description = (
                f"**CRASH**\n\n"
                f"📉 Market crash! You taxed **{market_crash_count}** fellow top-3 players.\n"
                f"Total collected: **{market_crash_total}** {JOPACOIN_EMOTE}\n\n"
                f"*The rich get richer. That's just economics.*"
            )

    elif value == "COMPOUND_INTEREST":
        title = f"📈 +{compound_amount} BONUS! 📈"
        color = discord.Color.from_str("#6b8c00")
        description = (
            f"**+{compound_amount}**\n\n"
            f"📈 A flat bonus, straight to your balance.\n"
            f"Earned **{compound_amount}** {JOPACOIN_EMOTE}.\n\n"
            f"*The most boring way to get rich — and the most reliable.*"
        )

    elif value == "TRICKLE_DOWN":
        title = "💧 TRICKLE DOWN! 💧"
        color = discord.Color.from_str("#5c7a00")
        if trickle_count == 0:
            description = (
                "**TRICKLE**\n\n"
                "Trickle down economics... but there's no one else to tax.\n\n"
                "*Nothing happens. As usual.*"
            )
        else:
            description = (
                f"**TRICKLE**\n\n"
                f"💧 You taxed **{trickle_count}** players 2-5% of their balance.\n"
                f"Total received: **{trickle_total}** {JOPACOIN_EMOTE}\n\n"
                f"*It trickled up, actually.*"
            )

    elif value == "DIVIDEND":
        title = "💎 DIVIDEND! 💎"
        color = discord.Color.from_str("#4a7000")
        description = (
            f"**DIVIDEND**\n\n"
            f"💎 The server's collective wealth pays out!\n"
            f"Earned **{dividend_amount}** {JOPACOIN_EMOTE} (0.5% of total guild wealth).\n\n"
            f"*Being rich in a rich server pays dividends. Literally.*"
        )

    elif value == "HOSTILE_TAKEOVER":
        title = "🏴 HOSTILE TAKEOVER! 🏴"
        color = discord.Color.from_str("#6a2a80")
        if takeover_missed:
            description = (
                f"**TAKEOVER**\n\n"
                f"🏴 You targeted rank #4... but they're broke or don't exist.\n\n"
                f"*Consolation prize: **+{takeover_amount}** {JOPACOIN_EMOTE}.*"
            )
        else:
            description = (
                f"**TAKEOVER**\n\n"
                f"🏴 Corporate raid on **{takeover_victim_name}** (rank #4)!\n"
                f"Seized **{takeover_amount}** {JOPACOIN_EMOTE}.\n\n"
                f"*So close to the top — and yet, so far.*"
            )

    elif value == "RECESSION":
        title = "🩸 RECESSION! 🩸"
        color = discord.Color.from_str("#3a0a0a")
        description = (
            f"**RECESSION**\n\n"
            f"📉 The economy contracts. **{recession_count}** players lost a slice of their balance — "
            f"the wealthier you are, the more you bleed.\n\n"
            f"You lost **{recession_self_loss}** {JOPACOIN_EMOTE}.\n"
            f"Total drained from the server: **{recession_total}** {JOPACOIN_EMOTE} (sent to Jopacoin Reserve).\n\n"
            f"*Pride goes before the fall — and it drags the rest down with it.*"
        )

    # --- Mario Kart deflation wedges (mostly burn coins from the economy) ---
    elif value == "BANANA_PEEL":
        title = "🍌 BANANA PEEL! 🍌"
        color = discord.Color.from_str("#ffe14a")
        if banana_missed:
            description = (
                "**BANANA**\n\n"
                "🍌 You dropped a banana peel… but no one was riding your bumper.\n\n"
                "*A wasted prank.*"
            )
        else:
            victim_display = banana_victim.mention if banana_victim else f"**{banana_victim_name}**"
            description = (
                f"**BANANA**\n\n"
                f"🍌 {victim_display} (ranked just below you) slipped on your peel!\n\n"
                f"They lost **{banana_victim_loss}** {JOPACOIN_EMOTE} — burned into the void.\n\n"
                f"*Stay behind a thrower at your own peril.*"
            )

    elif value == "GREEN_SHELL":
        title = "🟢 GREEN SHELL! 🟢"
        color = discord.Color.from_str("#228b22")
        if green_shell_missed:
            description = (
                "**GREEN**\n\n"
                "🐢 The shell skittered off the track — no one was around to clip.\n\n"
                "*Better luck next spin.*"
            )
        else:
            victim_display = green_shell_victim.mention if green_shell_victim else f"**{green_shell_victim_name}**"
            description = (
                f"**GREEN**\n\n"
                f"🐢 You launched a green shell at {victim_display}!\n\n"
                f"Stole **{green_shell_amount}** {JOPACOIN_EMOTE} for yourself.\n\n"
                f"*Direct hit. Pocket the coins.*"
            )

    elif value == "BOMB_OMB":
        title = "💣 BOMB-OMB! 💣"
        color = discord.Color.from_str("#0d0d0d")
        if bomb_omb_missed:
            description = (
                "**BOMB**\n\n"
                "💣 The bomb-omb rolled into an empty room and fizzled.\n\n"
                "*No one around to splash.*"
            )
        else:
            victim_lines = ""
            if bomb_omb_victims:
                for vname, vamt, _ in bomb_omb_victims:
                    victim_lines += f"💥 **{vname}** lost **{vamt}** {JOPACOIN_EMOTE}\n"
            description = (
                f"**BOMB**\n\n"
                f"💣 KABOOM! The bomb-omb detonates into the crowd.\n\n"
                f"{victim_lines}"
                f"Total burned: **{bomb_omb_burn_total}** {JOPACOIN_EMOTE} (vanished, not banked).\n\n"
                f"*You walked away unscathed. Lucky.*"
            )

    # --- Mana bonus wedge embeds ---
    elif value == "ERUPTION":
        title = "⛰️🔥 ERUPTION!"
        color = discord.Color.from_str("#ff4500")
        description = (
            "**ERUPTION**\n\n"
            "The Mountain erupts! You gain **2x** the last spinner's result.\n\n"
            "*Red mana burns bright.*"
        )

    elif value == "OVERGROWTH":
        title = "🌲🌿 OVERGROWTH!"
        color = discord.Color.from_str("#228b22")
        description = (
            "**OVERGROWTH**\n\n"
            "The Forest rewards consistency. You earn 10 JC per game played this week.\n\n"
            "*Slow and steady wins the race.*"
        )

    elif value == "DECAY":
        top_loss = scale_minigame_jc_delta(60)
        fourth_loss = scale_minigame_jc_delta(80)
        title = "🌿💀 DECAY!"
        color = discord.Color.from_str("#4b0082")
        description = (
            "**DECAY**\n\n"
            f"Rot spreads to the wealthy. The top 3 are targeted for {top_loss} JC each, "
            f"rank #4 for {fourth_loss} JC. You consume what lands.\n\n"
            "*The Swamp claims what it is owed.*"
        )

    elif isinstance(value, int) and value > 0:
        # Win
        if is_bankrupt and value == 1:
            title = "🪙 One Coin. One."
            color = discord.Color.from_str("#3a3a1a")
            description = "**1**\n\nThe wheel took pity on you. One coin.\n\n*It's still technically a win.*"
        elif is_bankrupt and value == 2:
            title = "🪙 Two Coins."
            color = discord.Color.from_str("#3a3500")
            description = "**2**\n\nEven charity has standards. Here's 2.\n\n*Don't spend it all in one place.*"
        elif is_golden and label == "CROWN":
            title = "👑 CROWN JEWEL! 👑"
            color = discord.Color.from_str("#ffd700")
            description = (
                f"**CROWN**\n\n"
                f"✨ **+{value} {JOPACOIN_EMOTE} JACKPOT!** ✨\n\n"
                f"The golden wheel's ultimate prize.\n"
                f"The crown jewel of Jopacoin fortune.\n\n"
                f"*The server weeps. You reign.*"
            )
        elif value == 100:
            title = "🌟 JACKPOT! 🌟"
            color = discord.Color.gold()
            description = f"**{label}**\n\nYou won **{value}** {JOPACOIN_EMOTE}!"
        elif is_golden:
            title = "👑 Golden Win!"
            color = discord.Color.from_str("#daa520")
            description = f"**+{value} JC**\n\nYou won **{value}** {JOPACOIN_EMOTE} from the Golden Wheel!"
        else:
            title = "🎉 Winner!"
            color = discord.Color.green()
            description = f"**+{value} JC**\n\nYou won **{value}** {JOPACOIN_EMOTE}!"

        if garnished > 0:
            description += f"\n\n*{garnished} {JOPACOIN_EMOTE} went to debt repayment.*"

    elif isinstance(value, int) and value < 0:
        if is_golden:
            # OVEREXTENDED — golden wheel's penalty wedge
            title = "📉 OVEREXTENDED! 📉"
            color = discord.Color.from_str("#4a3000")
            description = (
                f"**OVEREXTENDED**\n\n"
                f"You flew too close to the sun.\n\n"
                f"Lost **{abs(value)}** {JOPACOIN_EMOTE}.\n\n"
                f"*Pride goes before the fall.*"
            )
        else:
            title = "💀 BANKRUPT! 💀"
            color = discord.Color.red()
            description = (
                f"**{label}**\n\n"
                f"You lost **{abs(value)}** {JOPACOIN_EMOTE}!\n\n"
                f"*The wheel shows no mercy...*"
            )
    elif pardon_consumed:
        # COMEBACK pardon absorbed the BANKRUPT
        title = "🃏 CLUTCH ACTIVATED! — BANKRUPT SAVED 🃏"
        color = discord.Color.from_str("#0a1a2a")
        description = (
            "**CLUTCH**\n\n"
            "You were about to go BANKRUPT... but your CLUTCH token saved you. Treated as LOSE."
        )
    else:
        # Lose a Turn (0) - 5 day penalty cooldown
        title = "🚫 LOSE A TURN 🚫"
        color = discord.Color.dark_gray()
        description = (
            f"**{label}**\n\n"
            f"No jopacoin lost... but you just got **5-day timeout'd** from the wheel.\n\n"
            f"*Imagine being this unlucky. Go outside. Touch grass. "
            f"Reflect on your gambling addiction.*"
        )

    embed = discord.Embed(
        title=title,
        description=description,
        color=color,
    )

    if bankruptcy_penalty > 0:
        embed.add_field(
            name="Bankruptcy Penalty",
            value=f"−{bankruptcy_penalty} {JOPACOIN_EMOTE} withheld while bankrupt",
            inline=False,
        )

    if shield_absorbed_total > 0:
        embed.add_field(
            name="🌾 White Mana Shields",
            value=(
                f"{shielded_count} shield activation(s) absorbed "
                f"**{shield_absorbed_total}** {JOPACOIN_EMOTE}."
            ),
            inline=False,
        )

    embed.add_field(
        name="New Balance",
        value=f"**{new_balance}** {JOPACOIN_EMOTE}",
        inline=False,
    )

    embed.add_field(
        name="Next Spin",
        value=f"<t:{next_spin_time}:R>",
        inline=False,
    )

    return embed


def build_wheel_explosion_embed(
    new_balance: int, garnished: int, next_spin_time: int,
    reward: int = WHEEL_EXPLOSION_REWARD, bankruptcy_penalty: int = 0,
) -> discord.Embed:
    """Build the result embed when the wheel explodes."""
    title = "💥 THE WHEEL EXPLODED! 💥"
    color = discord.Color.orange()

    description = (
        f"**KABOOM!**\n\n"
        f"The wheel has exploded! Fortunately, no one was hurt.\n\n"
        f"We sincerely apologize for the inconvenience. "
        f"As compensation, you've been awarded **{reward}** {JOPACOIN_EMOTE}."
    )

    if garnished > 0:
        description += f"\n\n*{garnished} {JOPACOIN_EMOTE} went to debt repayment.*"

    embed = discord.Embed(
        title=title,
        description=description,
        color=color,
    )

    if bankruptcy_penalty > 0:
        embed.add_field(
            name="Bankruptcy Penalty",
            value=f"−{bankruptcy_penalty} {JOPACOIN_EMOTE} withheld while bankrupt",
            inline=False,
        )

    embed.add_field(
        name="New Balance",
        value=f"**{new_balance}** {JOPACOIN_EMOTE}",
        inline=False,
    )

    embed.add_field(
        name="Next Spin",
        value=f"<t:{next_spin_time}:R>",
        inline=False,
    )

    embed.set_footer(text="Our engineers are working on a replacement wheel.")

    return embed
