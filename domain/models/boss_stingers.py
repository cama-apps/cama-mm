"""Per-boss on-loss stinger effects.

A stinger is an additional penalty that fires when a player loses a boss fight,
on top of the baseline wager forfeit + depth knockback applied by
``services.dig_service.fight_boss``. Each boss has exactly one stinger (keyed
by ``stinger_id`` on the ``BossDef``).

Effects supported:
    - ``extra_knockback``: additional depth lost on top of the base 5-10 roll.
    - ``extended_cooldown_s``: seconds added to the dig cooldown for the next dig.
    - ``cursed_status``: a string key written into ``tunnels.stinger_curse`` that
      other systems check (e.g. ``halve_next_wager`` is checked at wager time).
    - ``flavor_on_loss``: 1-line narrative shown in the loss embed.
"""

from __future__ import annotations

from dataclasses import dataclass

# Cursed status keys that systems may check. Keep in sync with the applier sites.
CURSE_HALVE_NEXT_WAGER = "halve_next_wager"
CURSE_NO_SCOUT_NEXT_DIG = "no_scout_next_dig"
CURSE_DRAIN_NEXT_REWARD = "drain_next_reward"  # -25% JC on next boss win


@dataclass(frozen=True)
class StingerEffect:
    id: str
    extra_knockback: int
    extended_cooldown_s: int
    cursed_status: str | None
    flavor_on_loss: str


# Per-boss stingers. boss_id is encoded in the stinger id so content writers
# can reshape a single boss's stinger without touching shared archetype names.
STINGER_REGISTRY: dict[str, StingerEffect] = {
    # -------------------------- Tier 25 --------------------------
    "grothak_crumble": StingerEffect(
        id="grothak_crumble",
        extra_knockback=3,
        extended_cooldown_s=0,
        cursed_status=None,
        flavor_on_loss="Grothak slams a boulder down behind you as you flee.",
    ),
    "pudge_drag": StingerEffect(
        id="pudge_drag",
        extra_knockback=5,
        extended_cooldown_s=0,
        cursed_status=None,
        flavor_on_loss="Pudge drags you back toward his pile of meat.",
    ),
    "ogre_blast": StingerEffect(
        id="ogre_blast",
        extra_knockback=0,
        extended_cooldown_s=600,
        cursed_status=None,
        flavor_on_loss="Your ears ring for hours after Ogre's bellow.",
    ),
    # -------------------------- Tier 50 --------------------------
    "crystalia_shard": StingerEffect(
        id="crystalia_shard",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_DRAIN_NEXT_REWARD,
        flavor_on_loss="A crystal shard lodges under your skin — it pulses dully.",
    ),
    "cm_freeze": StingerEffect(
        id="cm_freeze",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_HALVE_NEXT_WAGER,
        flavor_on_loss="You thaw out slowly. Your nerve is shot.",
    ),
    "tusk_kick": StingerEffect(
        id="tusk_kick",
        extra_knockback=7,
        extended_cooldown_s=0,
        cursed_status=None,
        flavor_on_loss="Tusk's walrus kick launches you a long way back.",
    ),
    # -------------------------- Tier 75 --------------------------
    "magmus_burn": StingerEffect(
        id="magmus_burn",
        extra_knockback=0,
        extended_cooldown_s=900,
        cursed_status=None,
        flavor_on_loss="Your gear smolders long after you escape the heat.",
    ),
    "lina_scorch": StingerEffect(
        id="lina_scorch",
        extra_knockback=0,
        extended_cooldown_s=600,
        cursed_status=CURSE_DRAIN_NEXT_REWARD,
        flavor_on_loss="Lina's fire leaves scars that ache every payday.",
    ),
    "doom_brand": StingerEffect(
        id="doom_brand",
        extra_knockback=5,
        extended_cooldown_s=0,
        cursed_status=CURSE_HALVE_NEXT_WAGER,
        flavor_on_loss="Doom's brand refuses to fade. You dig scared.",
    ),
    # -------------------------- Tier 100 --------------------------
    "void_collapse": StingerEffect(
        id="void_collapse",
        extra_knockback=8,
        extended_cooldown_s=0,
        cursed_status=None,
        flavor_on_loss="The Void Warden folds the tunnel behind you.",
    ),
    "spectre_haunting": StingerEffect(
        id="spectre_haunting",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_NO_SCOUT_NEXT_DIG,
        flavor_on_loss="A spectral copy of yourself walks behind you now. Nothing feels certain.",
    ),
    "void_spirit_exile": StingerEffect(
        id="void_spirit_exile",
        extra_knockback=10,
        extended_cooldown_s=300,
        cursed_status=None,
        flavor_on_loss="You're dimensionally mispunched and come out several floors up.",
    ),
    # -------------------------- Tier 150 --------------------------
    "sporeling_rot": StingerEffect(
        id="sporeling_rot",
        extra_knockback=0,
        extended_cooldown_s=1200,
        cursed_status=None,
        flavor_on_loss="Spores colonize your lungs. You cough for a long while.",
    ),
    "treant_entangle": StingerEffect(
        id="treant_entangle",
        extra_knockback=3,
        extended_cooldown_s=600,
        cursed_status=None,
        flavor_on_loss="Roots bind your boots. Hard to pick up the pace.",
    ),
    "broodmother_webbing": StingerEffect(
        id="broodmother_webbing",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_HALVE_NEXT_WAGER,
        flavor_on_loss="You rip out of the web, leaving too much coin behind.",
    ),
    # -------------------------- Tier 200 --------------------------
    "chronofrost_stillness": StingerEffect(
        id="chronofrost_stillness",
        extra_knockback=0,
        extended_cooldown_s=1800,
        cursed_status=None,
        flavor_on_loss="Time stuck to you on the way out. You move slow for a while.",
    ),
    "void_chrono": StingerEffect(
        id="void_chrono",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_NO_SCOUT_NEXT_DIG,
        flavor_on_loss="Faceless Void rewinds the last few minutes — you forget the way down.",
    ),
    "weaver_unmake": StingerEffect(
        id="weaver_unmake",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_DRAIN_NEXT_REWARD,
        flavor_on_loss="Weaver pulled a thread from your timeline. Your next prize is thinner.",
    ),
    # -------------------------- Tier 275 --------------------------
    "nameless_erase": StingerEffect(
        id="nameless_erase",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_NO_SCOUT_NEXT_DIG,
        flavor_on_loss="You come out without a clear memory of the descent.",
    ),
    "oracle_fate": StingerEffect(
        id="oracle_fate",
        extra_knockback=0,
        extended_cooldown_s=0,
        cursed_status=CURSE_HALVE_NEXT_WAGER,
        flavor_on_loss="Oracle showed you a bad future. Your hand is shaking.",
    ),
    "terrorblade_sundering": StingerEffect(
        id="terrorblade_sundering",
        extra_knockback=5,
        extended_cooldown_s=600,
        cursed_status=CURSE_DRAIN_NEXT_REWARD,
        flavor_on_loss="Terrorblade swaps something of yours for something of his. You don't want to know what.",
    ),
}


def get_stinger(stinger_id: str) -> StingerEffect | None:
    return STINGER_REGISTRY.get(stinger_id)
