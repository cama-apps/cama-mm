"""Mid-fight reactive mechanics for /dig boss duels.

A ``BossMechanic`` represents a single "moment" inside a boss fight where the
player is forced to make a reactive choice. Each boss has a ``mechanic_pool``
of 4-5 mechanic ids on its ``BossDef``; exactly one is rolled per fight and
triggers at its configured round number, pausing the auto-resolve loop until
the player clicks an option.

Each mechanic has exactly 3 ``MechanicOption``s. Each option has a tuple of
``OutcomeRoll``s whose probabilities sum to 1.0 — when the player clicks the
option we roll the distribution and apply the chosen ``OutcomeRoll`` to the
duel state (player/boss HP deltas, skip-next-round, status effects).

Status effects are implemented as tiny pure functions in ``EFFECT_APPLIERS``
so content writers can add new flavors without touching combat code. The
convention is: the applier receives the duel state (a plain dict of the same
shape persisted in ``dig_active_duels``) and returns the mutated state.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Literal

# ---------------------------------------------------------------------------
# Dataclasses
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class OutcomeRoll:
    """One branch of an option's probability distribution."""
    probability: float
    player_hp_delta: int                # negative = damage, positive = heal
    boss_hp_delta: int                  # negative = damage to boss
    skip_next_round_for: Literal["player", "boss", None]
    status_effect: str | None           # key into EFFECT_APPLIERS
    narrative: str                      # 1-line shown when this branch rolls


@dataclass(frozen=True)
class MechanicOption:
    """One of the three reactive buttons shown to the player."""
    label: str                          # button text (short)
    flavor: str                         # 1-line shown immediately on click
    outcome_rolls: tuple[OutcomeRoll, ...]  # probabilities must sum to 1.0


@dataclass(frozen=True)
class BossMechanic:
    """A full mid-fight prompt: title + description + 3 option buttons."""
    id: str                             # globally unique, e.g. "pudge_hook"
    archetype: str                      # e.g. "hook_pull" (shape family)
    trigger_round: int                  # round at which this fires if rolled
    prompt_title: str                   # big-text title shown on the prompt
    prompt_description: str             # 1-2 line narrative below the title
    options: tuple[MechanicOption, ...] # exactly 3 options
    safe_option_idx: int                # timeout/abandon fallback


# ---------------------------------------------------------------------------
# Effect appliers
# ---------------------------------------------------------------------------

# Duel state keys touched by appliers (and by the service state machine):
#   player_hp, boss_hp, round_num, status_effects (dict of str -> Any)

EffectApplier = Callable[[dict[str, Any]], dict[str, Any]]


def _apply_burn(state: dict[str, Any]) -> dict[str, Any]:
    """Burns deal 1 player damage per round for the next 2 rounds."""
    effects = dict(state.get("status_effects") or {})
    effects["burn_rounds_remaining"] = 2
    state["status_effects"] = effects
    return state


def _apply_silence(state: dict[str, Any]) -> dict[str, Any]:
    """Silenced: player deals half damage next round (rounded down)."""
    effects = dict(state.get("status_effects") or {})
    effects["silenced_next_round"] = True
    state["status_effects"] = effects
    return state


def _apply_bleed(state: dict[str, Any]) -> dict[str, Any]:
    """Bleed: player takes 1 damage per round for the next 3 rounds."""
    effects = dict(state.get("status_effects") or {})
    effects["bleed_rounds_remaining"] = 3
    state["status_effects"] = effects
    return state


def _apply_frostbite(state: dict[str, Any]) -> dict[str, Any]:
    """Frostbitten: boss gets +1 hit chance next round (interpreted as extra dmg)."""
    effects = dict(state.get("status_effects") or {})
    effects["frostbite_next_round"] = True
    state["status_effects"] = effects
    return state


def _apply_reveal(state: dict[str, Any]) -> dict[str, Any]:
    """Revealed: boss loses a flat 1 HP at start of next round (exposed)."""
    effects = dict(state.get("status_effects") or {})
    effects["boss_exposed_next_round"] = True
    state["status_effects"] = effects
    return state


EFFECT_APPLIERS: dict[str, EffectApplier] = {
    "burn": _apply_burn,
    "silence": _apply_silence,
    "bleed": _apply_bleed,
    "frostbite": _apply_frostbite,
    "reveal": _apply_reveal,
}


# ---------------------------------------------------------------------------
# Mechanic registry (archetypes + per-boss instances)
# ---------------------------------------------------------------------------
# CONTENT NOTES
#
# The registry starts with a compact set of primary mechanics — one per boss
# (21 total). This is enough to boot the state machine, run tests, and play
# the feature end to end. The plan calls for 4-5 mechanics per boss
# (~95 total); the remaining ~75 instances are written by parallel agents
# after this baseline lands. Expansion only touches THIS file + BossDef.mechanic_pool
# tuples in dig_constants.py — no state-machine or UI code needs to change.
#
# Shape conventions (documented for content authors):
#   - Exactly 3 options per mechanic
#   - Option probabilities for a single option's outcome_rolls must sum to 1.0
#   - trigger_round is typically between 2-6 (fights last <=20 rounds)
#   - narrative strings should be one sentence, present tense, <=100 chars
#   - safe_option_idx is the "don't do anything crazy" button — lowest variance.

MECHANIC_REGISTRY: dict[str, BossMechanic] = {

    # ================================================================
    # TIER 25
    # ================================================================
    "grothak_earthquake": BossMechanic(
        id="grothak_earthquake",
        archetype="channel_aoe",
        trigger_round=3,
        prompt_title="Grothak rears up for a slam",
        prompt_description="The cavern shudders. A boulder-sized fist rises.",
        options=(
            MechanicOption(
                label="Brace against the wall",
                flavor="You put your back to stone.",
                outcome_rolls=(
                    OutcomeRoll(0.75, -1, 0, None, None, "Dust fills your mouth but you stay on your feet."),
                    OutcomeRoll(0.25, -2, 0, None, None, "The wall cracks — a chunk catches you in the chest."),
                ),
            ),
            MechanicOption(
                label="Roll into his leg",
                flavor="You tuck and dive forward.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, None, "You get under the slam; your pick opens a gash on his shin."),
                    OutcomeRoll(0.45, -2,  0, "player", None, "The slam catches your shoulder — you lose your footing."),
                ),
            ),
            MechanicOption(
                label="Leap and swing for his face",
                flavor="You go for the throat.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -3, None, None, "You land a brutal blow on his jaw — teeth fly."),
                    OutcomeRoll(0.75, -3,  0, None, None, "Grothak catches you out of the air; you hit the floor hard."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "pudge_hook": BossMechanic(
        id="pudge_hook",
        archetype="hook_pull",
        trigger_round=3,
        prompt_title="Pudge winds up the hook",
        prompt_description="The chain rattles. His arm cocks back.",
        options=(
            MechanicOption(
                label="Dodge left",
                flavor="You dive for cover.",
                outcome_rolls=(
                    OutcomeRoll(0.70,  0, 0, None, None, "The hook whips past you."),
                    OutcomeRoll(0.30, -1, 0, None, None, "The hook clips your shoulder."),
                ),
            ),
            MechanicOption(
                label="Dodge right into a swing",
                flavor="You trade a graze for a counter.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -2, None,     None, "You take it low, land a free hit on his gut."),
                    OutcomeRoll(0.50, -2,  0, "player", None, "The hook lands clean — no counter possible."),
                ),
            ),
            MechanicOption(
                label="Grab the hook",
                flavor="You lunge for the chain.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -4, None, None, "You yank Pudge off balance — massive hit!"),
                    OutcomeRoll(0.75, -3,  0, None, None, "Pudge pulls harder — you take the weight."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "ogre_multicast": BossMechanic(
        id="ogre_multicast",
        archetype="channel_multi",
        trigger_round=4,
        prompt_title="Ogre Magi's club glows purple",
        prompt_description="He chants. Three orbs of lightning spark at the tip.",
        options=(
            MechanicOption(
                label="Hide behind rubble",
                flavor="You break line of sight.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, 0, None, None, "Two orbs hit rock; one clips your arm."),
                    OutcomeRoll(0.35, -2, 0, None, None, "The orbs curve around the rubble."),
                ),
            ),
            MechanicOption(
                label="Interrupt the chant",
                flavor="You sprint at him.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0,  -2, None, None,      "You crack his club hand; chant fizzles."),
                    OutcomeRoll(0.60, -2, 0,  None, "silence", "He finishes first — you're caught in the blast."),
                ),
            ),
            MechanicOption(
                label="Take it and counter",
                flavor="You plant your feet.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -3, None, None, "Burned but clear-headed, you punish him."),
                    OutcomeRoll(0.70, -3,  0, None, None, "The multicast lands — all three."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 50
    # ================================================================
    "crystalia_prism": BossMechanic(
        id="crystalia_prism",
        archetype="reality_warp",
        trigger_round=3,
        prompt_title="Crystalia refracts the light",
        prompt_description="Three copies of her appear. They all move together.",
        options=(
            MechanicOption(
                label="Attack the centre copy",
                flavor="You strike geometry itself.",
                outcome_rolls=(
                    OutcomeRoll(0.50, 0, -2, None, None, "The centre shatters — the real Crystalia flinches."),
                    OutcomeRoll(0.50, -2, 0, None, None, "You picked a prism. Glass shards fly at you."),
                ),
            ),
            MechanicOption(
                label="Close your eyes and listen",
                flavor="You trust your ears.",
                outcome_rolls=(
                    OutcomeRoll(0.70, 0,  -1, None, None, "You hear her breath and strike blind."),
                    OutcomeRoll(0.30, -1,  0, None, None, "She moves silently; you swing at nothing."),
                ),
            ),
            MechanicOption(
                label="Swing in a wide arc",
                flavor="You hit everything.",
                outcome_rolls=(
                    OutcomeRoll(0.35, 0,  -3, None, None, "Two prisms and the real her — clean sweep."),
                    OutcomeRoll(0.65, -2, -1, None, None, "Fragments everywhere. You cut her once but cost yourself."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "cm_frostbite": BossMechanic(
        id="cm_frostbite",
        archetype="dot_debuff",
        trigger_round=2,
        prompt_title="Crystal Maiden chants a frostbite",
        prompt_description="Ice crawls up your boots. Your breath fogs.",
        options=(
            MechanicOption(
                label="Stomp the ice",
                flavor="You break it with force.",
                outcome_rolls=(
                    OutcomeRoll(0.65,  0, 0, None, None, "You shatter free before it sets."),
                    OutcomeRoll(0.35, -1, 0, None, "frostbite", "The ice grabs a foot; you limp."),
                ),
            ),
            MechanicOption(
                label="Close the distance",
                flavor="You rush her.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -2, None, None, "You cut her chant short. Worth the freeze."),
                    OutcomeRoll(0.55, -2,  0, None, "frostbite", "She finishes — your legs seize up."),
                ),
            ),
            MechanicOption(
                label="Stand still and wait",
                flavor="Ride it out.",
                outcome_rolls=(
                    OutcomeRoll(0.20,  0,  0, None, None, "She miscounts. The freeze fizzles."),
                    OutcomeRoll(0.80, -2,  0, "player", "frostbite", "You freeze solid. Turn wasted."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "tusk_snowball": BossMechanic(
        id="tusk_snowball",
        archetype="charge_telegraph",
        trigger_round=4,
        prompt_title="Tusk packs a snowball the size of a bison",
        prompt_description="He's rolling it faster than should be possible.",
        options=(
            MechanicOption(
                label="Sidestep the ball",
                flavor="You wait for the last second.",
                outcome_rolls=(
                    OutcomeRoll(0.60,  0, 0, None, None, "The ball blasts past you into the wall."),
                    OutcomeRoll(0.40, -2, 0, "player", None, "You mistimed — the ball clips you HARD."),
                ),
            ),
            MechanicOption(
                label="Smash the ball",
                flavor="You swing for a split.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -2, None, None, "Ice explodes; you tag Tusk through the spray."),
                    OutcomeRoll(0.55, -2,  0, None, None, "Ball holds together. It hits you like a truck."),
                ),
            ),
            MechanicOption(
                label="Ride the ball",
                flavor="You leap on top.",
                outcome_rolls=(
                    OutcomeRoll(0.30,  0, -3, None, None, "You surf the ball straight into Tusk — perfect hit."),
                    OutcomeRoll(0.70, -3,  0, None, None, "You slip off; the ball rolls over you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 75
    # ================================================================
    "magmus_eruption": BossMechanic(
        id="magmus_eruption",
        archetype="channel_aoe",
        trigger_round=5,
        prompt_title="Magmus Rex plunges his fist into the lava",
        prompt_description="The floor glows orange in a spreading ring.",
        options=(
            MechanicOption(
                label="Climb the wall",
                flavor="You scramble up.",
                outcome_rolls=(
                    OutcomeRoll(0.70,  0, 0, None, None, "You perch on a ledge as the eruption blows past."),
                    OutcomeRoll(0.30, -2, 0, None, "burn", "You slip mid-climb. The heat catches you."),
                ),
            ),
            MechanicOption(
                label="Sprint toward him",
                flavor="The centre of the ring is the safest spot.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -2, None, None, "You reach him through the pillars and land a hit."),
                    OutcomeRoll(0.50, -2, 0, None, "burn", "A geyser catches you dead-on."),
                ),
            ),
            MechanicOption(
                label="Dive into a cooling pool",
                flavor="You spot a dark puddle.",
                outcome_rolls=(
                    OutcomeRoll(0.35, +1, 0, None, None, "The water hisses but you come out healed."),
                    OutcomeRoll(0.65, -2, 0, None, "burn", "It was molten tar. Very much not cooling."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "lina_laguna": BossMechanic(
        id="lina_laguna",
        archetype="channel_big_hit",
        trigger_round=4,
        prompt_title="Lina charges the Laguna Blade",
        prompt_description="Lightning arcs between her fingertips. Her hair lifts.",
        options=(
            MechanicOption(
                label="Hide behind a stalagmite",
                flavor="You put rock between you.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None, "The bolt forks around the stone; some still catches you."),
                    OutcomeRoll(0.40, -3, 0, None, "burn", "It punches through — you're crisped."),
                ),
            ),
            MechanicOption(
                label="Charge her as she casts",
                flavor="The channel is long. You close.",
                outcome_rolls=(
                    OutcomeRoll(0.40,  0, -3, None, None, "You interrupt her mid-chant. Clean hit."),
                    OutcomeRoll(0.60, -3,  0, None, "burn", "You don't make it. The blade finishes."),
                ),
            ),
            MechanicOption(
                label="Hold up your pickaxe as a lightning rod",
                flavor="A thin hope.",
                outcome_rolls=(
                    OutcomeRoll(0.25, -1, -3, None, None, "The blade arcs up the pick and back into HER. Insane."),
                    OutcomeRoll(0.75, -3,  0, "player", "burn", "The pick shatters in your hand. You don't."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "doom_mark": BossMechanic(
        id="doom_mark",
        archetype="mark_delayed",
        trigger_round=3,
        prompt_title="Doom brands you with a black sigil",
        prompt_description="You feel it burn. He says: 'Silence. Doom approaches.'",
        options=(
            MechanicOption(
                label="Attack through it",
                flavor="Ignore the mark.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, None,      "You land hits. The mark pulses but holds."),
                    OutcomeRoll(0.45, -2,  0, None, "silence", "Mid-swing, the mark silences you."),
                ),
            ),
            MechanicOption(
                label="Try to burn it off",
                flavor="You scrape the sigil with your flame.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, 0, None, None, "The mark smolders out. Clean."),
                    OutcomeRoll(0.60, -2, 0, None, "bleed", "You burn yourself badly. The mark remains."),
                ),
            ),
            MechanicOption(
                label="Offer him something in trade",
                flavor="You toss a JC coin.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -3, None, None, "Doom laughs, removes the mark, then hits himself. Weird."),
                    OutcomeRoll(0.75, -2,  0, "player", None, "Doom accepts. The mark stays."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 100
    # ================================================================
    "voidwarden_collapse": BossMechanic(
        id="voidwarden_collapse",
        archetype="reality_warp",
        trigger_round=4,
        prompt_title="The Void Warden folds the room",
        prompt_description="Gravity tilts. The ceiling is below you now.",
        options=(
            MechanicOption(
                label="Accept the geometry",
                flavor="You fall up.",
                outcome_rolls=(
                    OutcomeRoll(0.55, 0, -2, None, None, "You land a kick from a weird angle — connects."),
                    OutcomeRoll(0.45, -2, 0, None, None, "Your aim is ruined. You swing at the floor."),
                ),
            ),
            MechanicOption(
                label="Close your eyes",
                flavor="You fight by feel.",
                outcome_rolls=(
                    OutcomeRoll(0.60, 0, -1, None, "reveal", "You find him by breath. He's exposed."),
                    OutcomeRoll(0.40, -2, 0, None, None, "You walk into a wall. It punches back."),
                ),
            ),
            MechanicOption(
                label="Throw yourself at the warp",
                flavor="Charge the eye of it.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -3, None, None, "You punch straight through. He fumbles."),
                    OutcomeRoll(0.70, -3, 0, None, None, "The warp eats you and spits you out winded."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "spectre_haunt": BossMechanic(
        id="spectre_haunt",
        archetype="stealth_strike",
        trigger_round=3,
        prompt_title="Spectre splits into copies",
        prompt_description="Shadow versions of her fan out around you.",
        options=(
            MechanicOption(
                label="Pick a copy at random",
                flavor="Full commit.",
                outcome_rolls=(
                    OutcomeRoll(0.33, 0, -3, None, None, "Lucky guess. The real Spectre reels."),
                    OutcomeRoll(0.67, -2, 0, None, "bleed", "The copy cuts you as it dissolves."),
                ),
            ),
            MechanicOption(
                label="Defensive stance — wait",
                flavor="Let them strike first.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, 0, None, "reveal", "The real Spectre's footfall is heavier."),
                    OutcomeRoll(0.30, -2, 0, None, "bleed", "They all hit at once. You can't block them all."),
                ),
            ),
            MechanicOption(
                label="Spin swing",
                flavor="Hit them all.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -2, None, None, "You sweep through two shadows and the real one."),
                    OutcomeRoll(0.60, -3, 0, "player", "bleed", "You miss the original. They counter from behind."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "void_spirit_step": BossMechanic(
        id="void_spirit_step",
        archetype="stealth_strike",
        trigger_round=3,
        prompt_title="Void Spirit steps sideways in time",
        prompt_description="He's there. Then he's not. Then he's there.",
        options=(
            MechanicOption(
                label="Predict the return point",
                flavor="You guess where.",
                outcome_rolls=(
                    OutcomeRoll(0.45, 0, -3, None, None, "Nailed it. He materializes on your pick."),
                    OutcomeRoll(0.55, -2, 0, None, None, "Wrong side. He appears behind you."),
                ),
            ),
            MechanicOption(
                label="Stand still and watch",
                flavor="Listen for the phase-in hum.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, -1, None, None, "You trade blows; you both get one."),
                    OutcomeRoll(0.40, -2, 0, None, None, "You were too slow. He got the first swing."),
                ),
            ),
            MechanicOption(
                label="Chase into the rift",
                flavor="Jump in after him.",
                outcome_rolls=(
                    OutcomeRoll(0.25, 0, -4, None, None, "You follow him and catch him in the void — pure hit."),
                    OutcomeRoll(0.75, -3, 0, "player", None, "The rift spits you somewhere wrong."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    # ================================================================
    # TIER 150
    # ================================================================
    "sporeling_cloud": BossMechanic(
        id="sporeling_cloud",
        archetype="dot_debuff",
        trigger_round=3,
        prompt_title="Sporeling Sovereign releases a spore cloud",
        prompt_description="The air turns thick and sweet.",
        options=(
            MechanicOption(
                label="Hold your breath",
                flavor="Don't inhale.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None, "You hold on. Barely."),
                    OutcomeRoll(0.40, -2, 0, None, "bleed", "You gasp. Spores root in your lungs."),
                ),
            ),
            MechanicOption(
                label="Set it on fire",
                flavor="Torch the air itself.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -2, None, None, "Clean burn. The whole mass lights up."),
                    OutcomeRoll(0.50, -3, 0, None, "burn", "You blew yourself up. Not ideal."),
                ),
            ),
            MechanicOption(
                label="Breathe it in",
                flavor="Become the spore.",
                outcome_rolls=(
                    OutcomeRoll(0.20, +1, -2, None, None, "You attune to the mycelium — it helps you, somehow."),
                    OutcomeRoll(0.80, -3, 0, "player", "bleed", "You're a spore garden now. It hurts."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "treant_overgrowth": BossMechanic(
        id="treant_overgrowth",
        archetype="bind_debuff",
        trigger_round=3,
        prompt_title="Treant Protector grows roots around you",
        prompt_description="The ground erupts with vines.",
        options=(
            MechanicOption(
                label="Cut through the roots",
                flavor="Pickaxe work.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, 0, None, None, "You chop free in a few swings."),
                    OutcomeRoll(0.35, -2, 0, "player", None, "Roots keep regrowing. You spend the round."),
                ),
            ),
            MechanicOption(
                label="Climb the vines",
                flavor="Go up instead of through.",
                outcome_rolls=(
                    OutcomeRoll(0.45, 0, -2, None, None, "You get above and slash down at his crown."),
                    OutcomeRoll(0.55, -2, 0, None, "bleed", "The vines whip you down. Thorns everywhere."),
                ),
            ),
            MechanicOption(
                label="Call the roots to yourself",
                flavor="Bluff mycology.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -3, None, "reveal", "The vines bind Treant instead. Confusing but fine."),
                    OutcomeRoll(0.75, -3, 0, None, None, "Treant does not take kindly to bad mimicry."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "broodmother_spawn": BossMechanic(
        id="broodmother_spawn",
        archetype="summon_swarm",
        trigger_round=4,
        prompt_title="Broodmother births a brood of spiderlings",
        prompt_description="A dozen fist-sized spiders skitter toward your legs.",
        options=(
            MechanicOption(
                label="Stomp them all",
                flavor="Wide kicks.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None, "Crunch. Crunch. Crunch. You kill most."),
                    OutcomeRoll(0.45, -2, 0, None, "bleed", "Bites everywhere. They were faster than you."),
                ),
            ),
            MechanicOption(
                label="Let them pass — attack mama",
                flavor="Push through.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -2, -2, None, None, "They bite you but you reach her. Trade."),
                    OutcomeRoll(0.55, -3, 0, None, "bleed", "Too many bites. You don't reach her."),
                ),
            ),
            MechanicOption(
                label="Stand perfectly still",
                flavor="They hunt by vibration.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, 0, None, None, "They skitter past. Mama is confused."),
                    OutcomeRoll(0.60, -2, 0, "player", "bleed", "One of them noticed. Then all of them."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 200
    # ================================================================
    "chronofrost_still": BossMechanic(
        id="chronofrost_still",
        archetype="time_skip",
        trigger_round=4,
        prompt_title="Chronofrost freezes time around you",
        prompt_description="You can see his breath. You cannot see yours.",
        options=(
            MechanicOption(
                label="Fight the stillness",
                flavor="You force a step.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -1, None, None, "You break out and land a sluggish hit."),
                    OutcomeRoll(0.60, -2, 0, "player", "frostbite", "You stay frozen. He doesn't."),
                ),
            ),
            MechanicOption(
                label="Sit down and wait",
                flavor="Ride it out.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -2, 0, None, None, "He hits you but uses no big ability."),
                    OutcomeRoll(0.40, -3, 0, "player", None, "He had all the time in the world."),
                ),
            ),
            MechanicOption(
                label="Turn his time against him",
                flavor="You try to step between ticks.",
                outcome_rolls=(
                    OutcomeRoll(0.20,  0, -4, None, None, "You find a seam in the stop and stab it wide."),
                    OutcomeRoll(0.80, -3, 0, "player", "frostbite", "Time punishes arrogance. You seize up."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "faceless_void_chrono": BossMechanic(
        id="faceless_void_chrono",
        archetype="time_skip",
        trigger_round=5,
        prompt_title="Faceless Void casts Chronosphere",
        prompt_description="A dome of stopped time rises around you both.",
        options=(
            MechanicOption(
                label="Attack wildly inside the sphere",
                flavor="He's immune. You still have to try.",
                outcome_rolls=(
                    OutcomeRoll(0.35,  0, -2, None, None, "You clip him in the gap between ticks."),
                    OutcomeRoll(0.65, -3, 0, "player", None, "He takes his time with you."),
                ),
            ),
            MechanicOption(
                label="Run for the edge",
                flavor="Get outside the sphere.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None, "You stumble out. He follows but the chrono breaks."),
                    OutcomeRoll(0.45, -3, 0, "player", None, "You don't make it. He makes sure."),
                ),
            ),
            MechanicOption(
                label="Kneel and close your eyes",
                flavor="Refuse the fight.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, 0, None, None, "He finds that uninteresting and lets the chrono lapse."),
                    OutcomeRoll(0.70, -2, 0, "player", None, "He uses the time productively, mostly on you."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "weaver_timelapse": BossMechanic(
        id="weaver_timelapse",
        archetype="rewind",
        trigger_round=6,
        prompt_title="Weaver Time Lapses",
        prompt_description="He phases away and reappears at full HP a moment ago.",
        options=(
            MechanicOption(
                label="Attack as he snaps back",
                flavor="Read the line.",
                outcome_rolls=(
                    OutcomeRoll(0.50, 0, -2, None, None, "You hit him the instant he reforms."),
                    OutcomeRoll(0.50, -2, 0, None, None, "You misread the lapse direction."),
                ),
            ),
            MechanicOption(
                label="Accept the heal and wait",
                flavor="Let him reset.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, 0, None, None, "He returns but has to rebuild his attack. You buy time."),
                    OutcomeRoll(0.30, -2, 0, None, None, "He reappears already swinging."),
                ),
            ),
            MechanicOption(
                label="Grab a thread of his time",
                flavor="Rewind with him.",
                outcome_rolls=(
                    OutcomeRoll(0.25, +1, -3, None, None, "You end up in the past with him — you HEAL and catch him off guard."),
                    OutcomeRoll(0.75, -3, 0, None, None, "You let go too late. The thread slices you open."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    # ================================================================
    # TIER 275
    # ================================================================
    "nameless_whisper": BossMechanic(
        id="nameless_whisper",
        archetype="reality_warp",
        trigger_round=5,
        prompt_title="The Nameless Depth whispers your name",
        prompt_description="It sounds like your own voice. It knows things you don't.",
        options=(
            MechanicOption(
                label="Answer it",
                flavor="Speak back.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, -2, None, "reveal", "You name it in return. It flinches."),
                    OutcomeRoll(0.60, -2, 0, None, None, "Your voice wavers. It takes that wavering."),
                ),
            ),
            MechanicOption(
                label="Refuse the name",
                flavor="You are not that person.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None, "The whisper loses purchase and fades."),
                    OutcomeRoll(0.45, -2, 0, "player", None, "It was the right name. The refusal costs you."),
                ),
            ),
            MechanicOption(
                label="Offer a new name",
                flavor="Give it a false one.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -3, None, None, "It accepts the lie. Takes that name and leaves."),
                    OutcomeRoll(0.70, -3, 0, None, None, "It never accepted false names. It is upset."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "oracle_fortune": BossMechanic(
        id="oracle_fortune",
        archetype="gamble",
        trigger_round=4,
        prompt_title="Oracle casts Fortune's End on you",
        prompt_description="A coin spins between you. It is both sides at once.",
        options=(
            MechanicOption(
                label="Call heads",
                flavor="You commit.",
                outcome_rolls=(
                    OutcomeRoll(0.50, +1, -2, None, None, "You win. The fate bends your way."),
                    OutcomeRoll(0.50, -3,  0, None, None, "You lose. The fate bends hers."),
                ),
            ),
            MechanicOption(
                label="Call tails",
                flavor="Statistically equivalent.",
                outcome_rolls=(
                    OutcomeRoll(0.50, +1, -2, None, None, "You win. The fate bends your way."),
                    OutcomeRoll(0.50, -3,  0, None, None, "You lose. The fate bends hers."),
                ),
            ),
            MechanicOption(
                label="Refuse to call",
                flavor="You palm the coin.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -1, None, None, "She takes a neutral price. Both of you pay."),
                    OutcomeRoll(0.60, -2, 0, None, None, "Oracle dislikes unresolved fortunes."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),

    "terrorblade_sunder": BossMechanic(
        id="terrorblade_sunder",
        archetype="hp_swap",
        trigger_round=5,
        prompt_title="Terrorblade activates Sunder",
        prompt_description="He's at high HP. You are not. He wants to trade.",
        options=(
            MechanicOption(
                label="Let the sunder land",
                flavor="Accept the swap.",
                outcome_rolls=(
                    OutcomeRoll(1.00, +3, +2, None, None, "You swap HPs. You're healthier — he is healthier-than-before too, actually."),
                ),
            ),
            MechanicOption(
                label="Interrupt the sunder",
                flavor="You lunge at him.",
                outcome_rolls=(
                    OutcomeRoll(0.35, 0, -3, None, None, "You break his concentration."),
                    OutcomeRoll(0.65, -3, 0, None, None, "Sunder lands and cuts you both. He does better out of it."),
                ),
            ),
            MechanicOption(
                label="Reflect the sunder",
                flavor="Hold up your own pick.",
                outcome_rolls=(
                    OutcomeRoll(0.25, +2, -3, None, None, "It bounces. He gets sundered by himself. He's displeased."),
                    OutcomeRoll(0.75, -3, 0, "player", None, "Reflection was a lie. You take it in the chest."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
}


# ---------------------------------------------------------------------------
# Lookups
# ---------------------------------------------------------------------------

def get_mechanic(mechanic_id: str) -> BossMechanic | None:
    return MECHANIC_REGISTRY.get(mechanic_id)


def pick_random_mechanic_id(pool: tuple[str, ...], rng) -> str:
    """Pick a mechanic id from a boss's pool using the provided Random."""
    return rng.choice(pool)


def validate_mechanic(mechanic: BossMechanic) -> None:
    """Sanity-check invariants. Call from tests or at module import for safety."""
    if len(mechanic.options) != 3:
        raise ValueError(f"{mechanic.id}: expected 3 options, got {len(mechanic.options)}")
    if not 0 <= mechanic.safe_option_idx < 3:
        raise ValueError(f"{mechanic.id}: safe_option_idx out of range")
    for idx, option in enumerate(mechanic.options):
        total = sum(r.probability for r in option.outcome_rolls)
        if abs(total - 1.0) > 1e-6:
            raise ValueError(
                f"{mechanic.id}.options[{idx}]: probabilities sum to {total}, not 1.0"
            )
        for roll in option.outcome_rolls:
            if roll.status_effect and roll.status_effect not in EFFECT_APPLIERS:
                raise ValueError(
                    f"{mechanic.id}.options[{idx}]: unknown status_effect {roll.status_effect!r}"
                )
