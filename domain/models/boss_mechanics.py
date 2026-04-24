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
        prompt_title="The Butcher winds up the hook",
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
                    OutcomeRoll(0.25,  0, -4, None, None, "You yank The Butcher off balance — massive hit!"),
                    OutcomeRoll(0.75, -3,  0, None, None, "The Butcher pulls harder — you take the weight."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "ogre_multicast": BossMechanic(
        id="ogre_multicast",
        archetype="channel_multi",
        trigger_round=4,
        prompt_title="The Twin-Skulled's club glows purple",
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
                    OutcomeRoll(0.70, -3,  0, None, None, "The triple-cast lands — all three."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "grothak_crumble_wall": BossMechanic(
        id="grothak_crumble_wall",
        archetype="channel_aoe",
        trigger_round=4,
        prompt_title="Grothak headbutts the cavern wall",
        prompt_description="The wall cracks. A ton of rock starts sliding down.",
        options=(
            MechanicOption(
                label="Shoulder-check him into it",
                flavor="Return his gift.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -3, None, None, "You ram him into the slide — rocks bury his leg."),
                    OutcomeRoll(0.45, -2, -1, None, None, "You both catch stone. He gets the worst of it."),
                    OutcomeRoll(0.15, -3,  0, "player", None, "He doesn't budge. You bounce off and get buried."),
                ),
            ),
            MechanicOption(
                label="Dive under a ledge",
                flavor="You flatten against the floor.",
                outcome_rolls=(
                    OutcomeRoll(0.70,  0, 0, None, None, "Rocks pile on the ledge above you. You crawl out clean."),
                    OutcomeRoll(0.30, -2, 0, "player", None, "The ledge gives out. You're pinned for the round."),
                ),
            ),
            MechanicOption(
                label="Sprint straight through the slide",
                flavor="Outrun the landslide.",
                outcome_rolls=(
                    OutcomeRoll(0.35,  0, -1, None, None, "You clear it; a loose rock thwacks Grothak on the way."),
                    OutcomeRoll(0.65, -3,  0, None, "bleed", "You trip. A cascade of rock rolls over your back."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "pudge_rot": BossMechanic(
        id="pudge_rot",
        archetype="dot_debuff",
        trigger_round=2,
        prompt_title="The Butcher belches a cloud of rot",
        prompt_description="A green miasma rolls off his belly toward you.",
        options=(
            MechanicOption(
                label="Back off through it",
                flavor="Retreat on foot.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None,    "You cough your way out. Mostly clear."),
                    OutcomeRoll(0.45, -2, 0, None, "bleed", "The rot eats through your gloves."),
                ),
            ),
            MechanicOption(
                label="Push into the cloud",
                flavor="He can't rot himself.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -2, None, None,    "He flinches from his own stench; you land a hit."),
                    OutcomeRoll(0.55, -2,  0, None, "bleed", "Wrong — he loves his stench. You take the brunt."),
                ),
            ),
            MechanicOption(
                label="Ignite the cloud",
                flavor="Toss a torch.",
                outcome_rolls=(
                    OutcomeRoll(0.30,  0, -3, None, None,   "Whoof. The cloud lights up and blasts The Butcher back."),
                    OutcomeRoll(0.70, -3,  0, None, "burn", "It wasn't flammable. You were. Somehow."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "ogre_fireblast": BossMechanic(
        id="ogre_fireblast",
        archetype="channel_big_hit",
        trigger_round=3,
        prompt_title="The Twin-Skulled chants a slow fire blast",
        prompt_description="Left head counts down. Right head forgot the number.",
        options=(
            MechanicOption(
                label="Slap the left head",
                flavor="Interrupt the smart one.",
                outcome_rolls=(
                    OutcomeRoll(0.50,  0, -2, None, None,      "Left head loses count. The spell fizzles on him."),
                    OutcomeRoll(0.50, -2,  0, None, "silence", "Right head finishes the chant anyway."),
                ),
            ),
            MechanicOption(
                label="Confuse both heads",
                flavor="Shout nonsense at them.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, -1, None, None,    "They argue mid-cast; you both eat a weak spark."),
                    OutcomeRoll(0.40, -2,  0, None, "burn",  "They ignore you. The blast lands."),
                ),
            ),
            MechanicOption(
                label="Stand in front and grin",
                flavor="Bet on the miscast.",
                outcome_rolls=(
                    OutcomeRoll(0.25, +1, -3, None, None,   "Right head casts backwards. Ogre lights himself up."),
                    OutcomeRoll(0.75, -3,  0, None, "burn", "They both cast correctly for once. Disaster."),
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
        prompt_title="The Frostbinder chants a frostbite",
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
        prompt_title="the Warlord packs a snowball the size of a bison",
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
                    OutcomeRoll(0.45, -1, -2, None, None, "Ice explodes; you tag the Warlord through the spray."),
                    OutcomeRoll(0.55, -2,  0, None, None, "Ball holds together. It hits you like a truck."),
                ),
            ),
            MechanicOption(
                label="Ride the ball",
                flavor="You leap on top.",
                outcome_rolls=(
                    OutcomeRoll(0.30,  0, -3, None, None, "You surf the ball straight into the Warlord — perfect hit."),
                    OutcomeRoll(0.70, -3,  0, None, None, "You slip off; the ball rolls over you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "crystalia_shatter": BossMechanic(
        id="crystalia_shatter",
        archetype="charge_telegraph",
        trigger_round=4,
        prompt_title="Crystalia grows a barrage of shards",
        prompt_description="A ring of dagger-like crystals levitates, tips pointed at you.",
        options=(
            MechanicOption(
                label="Dive between the shards",
                flavor="Thread the needle.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None,   "You weave through; one grazes your ribs."),
                    OutcomeRoll(0.45, -2, 0, None, "bleed", "One catches you high on the shoulder."),
                ),
            ),
            MechanicOption(
                label="Shatter a shard mid-flight",
                flavor="Knock one into the others.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -2, None, None,   "Chain reaction — shards redirect into her flank."),
                    OutcomeRoll(0.50, -2,  0, None, "bleed", "You crack one; the rest still find you."),
                    OutcomeRoll(0.10,  0, -3, None, None,   "Perfect shot. The whole barrage ricochets home."),
                ),
            ),
            MechanicOption(
                label="Mirror the barrage back",
                flavor="Pickaxe as shield.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -3, None, None,   "A flawless reflection. She staggers."),
                    OutcomeRoll(0.75, -3,  0, None, "bleed", "The pick can't hold. Shards sheer through it."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "cm_freezing_field": BossMechanic(
        id="cm_freezing_field",
        archetype="channel_multi",
        trigger_round=5,
        prompt_title="The Frostbinder unleashes Freezing Field",
        prompt_description="Ice bombs detonate randomly in a wide ring around her.",
        options=(
            MechanicOption(
                label="Stay at the outer edge",
                flavor="Dance the perimeter.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None,        "You ride the edge; only the shockwaves clip you."),
                    OutcomeRoll(0.45, -2, 0, None, "frostbite", "An outer bomb catches your heel."),
                ),
            ),
            MechanicOption(
                label="Zigzag toward her",
                flavor="Commit to the kill.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -1, -3, None, None,        "You reach her through the barrage — big crack to the jaw."),
                    OutcomeRoll(0.65, -3,  0, None, "frostbite", "Two bombs land close. You go face-first into slush."),
                ),
            ),
            MechanicOption(
                label="Hug her — bombs miss point-blank",
                flavor="Into her bubble.",
                outcome_rolls=(
                    OutcomeRoll(0.30,  0, -2, None, None,      "No bomb lands inside. She panics and flails."),
                    OutcomeRoll(0.70, -2,  0, "player", None, "The bubble shifts. You end up in a crater."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "tusk_walrus_punch": BossMechanic(
        id="tusk_walrus_punch",
        archetype="charge_telegraph",
        trigger_round=3,
        prompt_title="the Warlord cocks back a tusked strike",
        prompt_description="His whole body winds up. His fist glows cyan.",
        options=(
            MechanicOption(
                label="Duck the uppercut",
                flavor="Hit the deck.",
                outcome_rolls=(
                    OutcomeRoll(0.65,  0, 0, None, None,    "You drop under it. His arm whiffs overhead."),
                    OutcomeRoll(0.35, -2, 0, None, None,    "His follow-through catches your back."),
                ),
            ),
            MechanicOption(
                label="Counter-punch his chin",
                flavor="Fist meets fist.",
                outcome_rolls=(
                    OutcomeRoll(0.35,  0, -3, None, None,   "You rock him first. He crumples."),
                    OutcomeRoll(0.55, -3,  0, None, None,   "He wins the exchange, brutally."),
                    OutcomeRoll(0.10, -1, -1, None, None,   "Clash. You both stagger."),
                ),
            ),
            MechanicOption(
                label="Let him connect, ride it",
                flavor="Roll with the hit.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, 0, None, None,       "You turn with it. Barely a scratch."),
                    OutcomeRoll(0.60, -2, 0, "player", None,   "He launches you skyward. You land wrong."),
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
        prompt_title="the Scorchwitch charges a crackling lightning lance",
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
        prompt_title="The Deathbringer brands you with a black sigil",
        prompt_description="You feel it burn. He says: 'Silence. The Deathbringer approaches.'",
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
                    OutcomeRoll(0.25,  0, -3, None, None, "The Deathbringer laughs, removes the mark, then hits himself. Weird."),
                    OutcomeRoll(0.75, -2,  0, "player", None, "The Deathbringer accepts. The mark stays."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "magmus_meteor": BossMechanic(
        id="magmus_meteor",
        archetype="mark_delayed",
        trigger_round=4,
        prompt_title="Magmus Rex marks you for a meteor",
        prompt_description="A red crosshair paints the ground at your feet.",
        options=(
            MechanicOption(
                label="Sprint out of the circle",
                flavor="Full sprint sideways.",
                outcome_rolls=(
                    OutcomeRoll(0.60,  0, 0, None, None,    "You clear it. The meteor cratrs empty ground."),
                    OutcomeRoll(0.40, -2, 0, None, "burn", "Close call — the shockwave scorches your flank."),
                ),
            ),
            MechanicOption(
                label="Drag him into the circle",
                flavor="Bait his own rock.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -2, -3, None, None,   "Both of you eat it. He takes far worse."),
                    OutcomeRoll(0.50, -3,  0, None, "burn", "He doesn't budge. You eat the meteor."),
                    OutcomeRoll(0.15,  0, -4, None, None,   "He stumbles in. Direct hit. He's stunned."),
                ),
            ),
            MechanicOption(
                label="Meet it with your pick raised",
                flavor="Block the sky.",
                outcome_rolls=(
                    OutcomeRoll(0.20, +1, -2, None, None,   "You split the meteor. Chunks tag him."),
                    OutcomeRoll(0.80, -3,  0, None, "burn", "The pick vaporizes. So does much of you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "lina_dragon_slave": BossMechanic(
        id="lina_dragon_slave",
        archetype="channel_aoe",
        trigger_round=3,
        prompt_title="the Scorchwitch conjures a rolling flame wave",
        prompt_description="A wave of dragon-shaped fire rolls down the corridor.",
        options=(
            MechanicOption(
                label="Flatten against the floor",
                flavor="Hug the ground.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,   "The fire passes over. You get a bit singed."),
                    OutcomeRoll(0.40, -2, 0, None, "burn", "It dips low at the wrong moment."),
                ),
            ),
            MechanicOption(
                label="Leap over the wave",
                flavor="Time the jump.",
                outcome_rolls=(
                    OutcomeRoll(0.45,  0, -1, None, None,   "You clear it clean and clip the Scorchwitch on the landing."),
                    OutcomeRoll(0.55, -3,  0, None, "burn", "Mistimed. The dragon's mouth catches you mid-air."),
                ),
            ),
            MechanicOption(
                label="Redirect with a swing",
                flavor="Bat the fire back.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -3, None, None,   "Impossibly, it works. the Scorchwitch gets scorched."),
                    OutcomeRoll(0.75, -3,  0, None, "burn", "Fire doesn't care about your pickaxe."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "doom_scorched_earth": BossMechanic(
        id="doom_scorched_earth",
        archetype="dot_debuff",
        trigger_round=4,
        prompt_title="The Deathbringer bathes the floor in infernal flame",
        prompt_description="Everywhere you step burns. He alone is untouched.",
        options=(
            MechanicOption(
                label="Keep moving — never stand still",
                flavor="Hot feet.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -1, None, None,   "You trade sparks with him in motion."),
                    OutcomeRoll(0.45, -2,  0, None, "burn", "He catches you on a pivot. Whole boot lights up."),
                ),
            ),
            MechanicOption(
                label="Stand on a rock pillar",
                flavor="Find a dry spot.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,    "You perch. The flame licks but doesn't climb."),
                    OutcomeRoll(0.40, -2, 0, "player", None, "The Deathbringer kicks the pillar. You tumble into the fire."),
                ),
            ),
            MechanicOption(
                label="Roll through and tackle him",
                flavor="Eat floor on the way.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -3, None, None,   "You drag him down. Roll him through his own flame."),
                    OutcomeRoll(0.70, -3,  0, None, "burn", "You roll into his boot. He was ready for that."),
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
        prompt_title="The Shade splits into copies",
        prompt_description="Shadow versions of her fan out around you.",
        options=(
            MechanicOption(
                label="Pick a copy at random",
                flavor="Full commit.",
                outcome_rolls=(
                    OutcomeRoll(0.33, 0, -3, None, None, "Lucky guess. The real The Shade reels."),
                    OutcomeRoll(0.67, -2, 0, None, "bleed", "The copy cuts you as it dissolves."),
                ),
            ),
            MechanicOption(
                label="Defensive stance — wait",
                flavor="Let them strike first.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, 0, None, "reveal", "The real The Shade's footfall is heavier."),
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
        prompt_title="the Astral Echo steps sideways in time",
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

    "voidwarden_silence": BossMechanic(
        id="voidwarden_silence",
        archetype="bind_debuff",
        trigger_round=3,
        prompt_title="The Void Warden drops a silent bubble",
        prompt_description="Inside the sphere, no sound exists. Your pick makes no impact.",
        options=(
            MechanicOption(
                label="Fight without sound",
                flavor="Feel the hits.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -2, None, None,      "You land two blind hits. One is very solid."),
                    OutcomeRoll(0.55, -2,  0, None, "silence", "You swing at echoes and miss."),
                ),
            ),
            MechanicOption(
                label="Step out of the sphere",
                flavor="Back to noise.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, 0, None, None,       "You escape the bubble clean."),
                    OutcomeRoll(0.35, -2, 0, "player", None,   "The sphere drags with you. You're pinned half-in."),
                ),
            ),
            MechanicOption(
                label="Scream into the silence",
                flavor="Will sound into being.",
                outcome_rolls=(
                    OutcomeRoll(0.25,  0, -3, None, "reveal", "Your voice cracks the bubble. He's exposed."),
                    OutcomeRoll(0.75, -3,  0, None, "silence", "The sphere swallows your scream. You're left gasping."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "spectre_dagger": BossMechanic(
        id="spectre_dagger",
        archetype="stealth_strike",
        trigger_round=2,
        prompt_title="The Shade throws a phantom dagger",
        prompt_description="A trail of shadow ink marks the dagger's flight.",
        options=(
            MechanicOption(
                label="Follow the trail back at her",
                flavor="Run the line.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -2, None, None,    "You sprint the trail and crash into her."),
                    OutcomeRoll(0.50, -2,  0, None, "bleed", "She vanishes mid-trail. You hit empty ink."),
                ),
            ),
            MechanicOption(
                label="Catch the dagger",
                flavor="Pluck it from the air.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -3, None, None,    "You grab it and throw it back, clean."),
                    OutcomeRoll(0.60, -2,  0, None, "bleed", "It slices your palm open."),
                    OutcomeRoll(0.10,  0, -2, None, None,    "The dagger hangs in air; you steal it mid-flight."),
                ),
            ),
            MechanicOption(
                label="Let the trail pass over you",
                flavor="Accept the slow.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -2, 0, None, None,    "The ink clings. You move slow but the dagger missed."),
                    OutcomeRoll(0.45, -2, 0, "player", "bleed", "The trail loops back. You're glued to the spot."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),

    "void_spirit_aether": BossMechanic(
        id="void_spirit_aether",
        archetype="reality_warp",
        trigger_round=4,
        prompt_title="the Astral Echo folds aether around you",
        prompt_description="A sphere of compressed dimension pins you in place.",
        options=(
            MechanicOption(
                label="Push the sphere's walls out",
                flavor="Widen the cage.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -1, None, None,     "You stretch it. You both take bruises."),
                    OutcomeRoll(0.45, -2,  0, "player", None, "You overextend. The sphere snaps back hard."),
                ),
            ),
            MechanicOption(
                label="Collapse the sphere inward",
                flavor="Let it crush.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -2, -3, None, None,   "You drag him in. The collapse hurts you both; him more."),
                    OutcomeRoll(0.65, -3,  0, None, None,   "Only you were inside. Compression costs you dearly."),
                ),
            ),
            MechanicOption(
                label="Stand still and meditate",
                flavor="Refuse the warp.",
                outcome_rolls=(
                    OutcomeRoll(0.50,  0, 0, None, "reveal", "Your stillness destabilizes it. He flickers."),
                    OutcomeRoll(0.50, -2, 0, None, None,    "The sphere tightens. Your ribs disagree."),
                ),
            ),
        ),
        safe_option_idx=2,
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
        prompt_title="the Elder Grove grows roots around you",
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
                    OutcomeRoll(0.25,  0, -3, None, "reveal", "The vines bind the Elder Grove instead. Confusing but fine."),
                    OutcomeRoll(0.75, -3, 0, None, None, "The Elder Grove does not take kindly to bad mimicry."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "broodmother_spawn": BossMechanic(
        id="broodmother_spawn",
        archetype="summon_swarm",
        trigger_round=4,
        prompt_title="The Nestmother births a brood of spiderlings",
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

    "sporeling_roots": BossMechanic(
        id="sporeling_roots",
        archetype="bind_debuff",
        trigger_round=4,
        prompt_title="Sporeling Sovereign threads mycelium around your ankles",
        prompt_description="White fungal threads braid up your legs, tightening fast.",
        options=(
            MechanicOption(
                label="Rip the threads with brute force",
                flavor="Muscle through.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None,     "You tear free; skin goes with it."),
                    OutcomeRoll(0.45, -2, 0, None, "bleed",  "The threads hook barbs in. They don't let go clean."),
                ),
            ),
            MechanicOption(
                label="Cut the mycelium at the source",
                flavor="Chop its root.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -2, None, None,    "You sever the main trunk. Sovereign howls."),
                    OutcomeRoll(0.60, -2,  0, "player", None, "The trunk regrows. You spend the round hacking."),
                ),
            ),
            MechanicOption(
                label="Let it grow — become rooted",
                flavor="Dig in.",
                outcome_rolls=(
                    OutcomeRoll(0.30, +1, -1, None, None,    "You anchor and swing from stability. Clean hit."),
                    OutcomeRoll(0.70, -3,  0, "player", "bleed", "The roots drink from you. You become garden."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "treant_leech_seed": BossMechanic(
        id="treant_leech_seed",
        archetype="dot_debuff",
        trigger_round=4,
        prompt_title="the Elder Grove plants a life seed in you",
        prompt_description="A hot bead burrows under your skin and starts drinking.",
        options=(
            MechanicOption(
                label="Dig the seed out",
                flavor="Knifepoint surgery.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -2, 0, None, None,    "You pry it out. It stung, but you're clean."),
                    OutcomeRoll(0.45, -3, 0, None, "bleed", "You cut too deep. The seed hangs on anyway."),
                ),
            ),
            MechanicOption(
                label="Feed it with a hit to the Grove",
                flavor="Spread the drain.",
                outcome_rolls=(
                    OutcomeRoll(0.40,  0, -2, None, None,   "The seed's tether reverses — he drinks from himself."),
                    OutcomeRoll(0.60, -2,  0, None, "bleed", "Your contact strengthens the bond. Bad trade."),
                ),
            ),
            MechanicOption(
                label="Ignore it and fight",
                flavor="Not his tempo.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -3, None, None,   "You outdamage the drain this round."),
                    OutcomeRoll(0.70, -3,  0, None, "bleed", "The seed drinks as you swing. You weaken fast."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "broodmother_web": BossMechanic(
        id="broodmother_web",
        archetype="bind_debuff",
        trigger_round=3,
        prompt_title="The Nestmother spins a web trap",
        prompt_description="Sticky silk criss-crosses the cavern at knee height.",
        options=(
            MechanicOption(
                label="Burn the web",
                flavor="Torch it.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -1, None, None,  "The web goes up. A flaming strand catches her too."),
                    OutcomeRoll(0.45, -2,  0, None, "burn", "The silk is oiled. You light yourself up."),
                ),
            ),
            MechanicOption(
                label="Crawl under on your belly",
                flavor="Go low.",
                outcome_rolls=(
                    OutcomeRoll(0.65,  0, 0, "player", None, "You slither out. You spent the round on the ground."),
                    OutcomeRoll(0.35, -2, 0, None,     None, "A lower strand catches your neck."),
                ),
            ),
            MechanicOption(
                label="Swing on a strand",
                flavor="Tarzan the web.",
                outcome_rolls=(
                    OutcomeRoll(0.30,  0, -3, None, None,    "You swing right into The Nestmother. Terrifying for her."),
                    OutcomeRoll(0.70, -3,  0, None, "bleed", "The strand snaps. Silk and fangs everywhere."),
                ),
            ),
        ),
        safe_option_idx=1,
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
        prompt_title="the Timeless One summons a time sphere",
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
        prompt_title="the Skitterwing rewinds the moment",
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

    "chronofrost_rewind": BossMechanic(
        id="chronofrost_rewind",
        archetype="rewind",
        trigger_round=5,
        prompt_title="Chronofrost rewinds his own wounds",
        prompt_description="Every scar on him un-stitches and vanishes.",
        options=(
            MechanicOption(
                label="Strike the seam of his rewind",
                flavor="Find the join.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -3, None, None,        "You hit the seam. His heal rebounds."),
                    OutcomeRoll(0.60, -3,  0, None, "frostbite", "You strike empty air. The rewind catches you instead."),
                ),
            ),
            MechanicOption(
                label="Force your own rewind",
                flavor="Ride the wave.",
                outcome_rolls=(
                    OutcomeRoll(0.30, +2, 0, None, None,        "Your wounds un-happen. You heal on his tick."),
                    OutcomeRoll(0.70, -2, 0, None, "frostbite", "You can't catch the thread. You age instead."),
                ),
            ),
            MechanicOption(
                label="Let him heal, swing anyway",
                flavor="Start fresh.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, None, "You don't interrupt; you just keep hitting him."),
                    OutcomeRoll(0.45, -2,  0, None, None, "He finishes the rewind and parries."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),

    "faceless_void_backtrack": BossMechanic(
        id="faceless_void_backtrack",
        archetype="rewind",
        trigger_round=3,
        prompt_title="the Timeless One Backtracks your attack",
        prompt_description="He rewinds a half-second. Your last swing un-happens.",
        options=(
            MechanicOption(
                label="Swing again harder",
                flavor="Twice the effort.",
                outcome_rolls=(
                    OutcomeRoll(0.45,  0, -2, None, None, "The second swing lands, past his rewind window."),
                    OutcomeRoll(0.55, -2,  0, None, None, "He backtracks again. You've wasted two rounds."),
                ),
            ),
            MechanicOption(
                label="Wait out the rewind",
                flavor="Let him run dry.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None, "He backtracks into nothing. You breathe easy."),
                    OutcomeRoll(0.40, -2, 0, "player", None, "He uses the free second to gut-punch you."),
                ),
            ),
            MechanicOption(
                label="Feint and follow through",
                flavor="Bait the rewind.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -1, -3, None, None, "He backtracks the feint. The real swing lands clean."),
                    OutcomeRoll(0.65, -3,  0, None, None, "He saw the feint coming because of course he did."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    "weaver_shukuchi": BossMechanic(
        id="weaver_shukuchi",
        archetype="stealth_strike",
        trigger_round=3,
        prompt_title="the Skitterwing flickers out of view",
        prompt_description="He phases invisible. Tiny mandibles click somewhere in the dark.",
        options=(
            MechanicOption(
                label="Swing where you heard the click",
                flavor="Ear target.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -2, None, None,     "Blind hit. He shimmers into view, bleeding."),
                    OutcomeRoll(0.60, -2,  0, None, None,     "You hit air. The clicks were a trick."),
                ),
            ),
            MechanicOption(
                label="Set the floor on fire",
                flavor="He has to step somewhere.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, "reveal", "He steps into flame; the flicker breaks."),
                    OutcomeRoll(0.45, -2,  0, None, "burn",   "He phased over. You scorched yourself."),
                ),
            ),
            MechanicOption(
                label="Stand dead still and listen",
                flavor="Make him come to you.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, 0, None, "reveal",   "He brushes past; you feel the draft. He's exposed."),
                    OutcomeRoll(0.50, -3, 0, "player", "bleed", "He phases through your back. Vicious."),
                ),
            ),
        ),
        safe_option_idx=2,
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
        prompt_title="the Seer calls down fate's edge on you",
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
                    OutcomeRoll(0.60, -2, 0, None, None, "the Seer dislikes unresolved fortunes."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),

    "terrorblade_sunder": BossMechanic(
        id="terrorblade_sunder",
        archetype="hp_swap",
        trigger_round=5,
        prompt_title="the Sundered Prince activates Sunder",
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
                    OutcomeRoll(0.65, -3, 0, None, None, "The soul-trade lands and cuts you both. He does better out of it."),
                ),
            ),
            MechanicOption(
                label="Reflect the sunder",
                flavor="Hold up your own pick.",
                outcome_rolls=(
                    OutcomeRoll(0.25, +2, -3, None, None, "It bounces. He gets sundered by himself. He's displeased."),
                    OutcomeRoll(0.75, -3, 0, "player", None, "The mirror was a lie. You take it in the chest."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "nameless_silence": BossMechanic(
        id="nameless_silence",
        archetype="bind_debuff",
        trigger_round=4,
        prompt_title="The Nameless Depth drinks all sound",
        prompt_description="Your heartbeat is inaudible. Even your thoughts dim.",
        options=(
            MechanicOption(
                label="Hum a childhood song",
                flavor="Hold onto something familiar.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -1, None, None,      "The hum holds. You both feel small."),
                    OutcomeRoll(0.45, -2,  0, None, "silence", "It takes the song too. You forget the tune."),
                ),
            ),
            MechanicOption(
                label="Shout your own name",
                flavor="Reassert you exist.",
                outcome_rolls=(
                    OutcomeRoll(0.40,  0, -2, None, "reveal",  "The Depth flinches; it cannot unknow you."),
                    OutcomeRoll(0.60, -2,  0, None, "silence", "It swallowed the name before you finished it."),
                ),
            ),
            MechanicOption(
                label="Stay silent and listen",
                flavor="Hear what it hears.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -1, -2, None, None,      "You catch its rhythm. Your pick finds the gap."),
                    OutcomeRoll(0.65, -3,  0, "player", "silence", "You lose your voice entirely for a round."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "oracle_false_promise": BossMechanic(
        id="oracle_false_promise",
        archetype="gamble",
        trigger_round=5,
        prompt_title="the Seer places a false vow on you",
        prompt_description="For a moment all damage is suspended. When it ends — everything resolves.",
        options=(
            MechanicOption(
                label="Attack recklessly during the promise",
                flavor="Nothing can hurt you... yet.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -2, -3, None, None, "You land huge hits. The promise ends. You pay some cost."),
                    OutcomeRoll(0.40, -3, -2, None, None, "Your damage taken accumulated. Still a decent trade."),
                    OutcomeRoll(0.15, -4,  0, None, None, "She extended the promise's back-end. It all hits you."),
                ),
            ),
            MechanicOption(
                label="Heal during the promise",
                flavor="Bank the hit points.",
                outcome_rolls=(
                    OutcomeRoll(0.55, +2, 0, None, None, "You pour potion into your wounds; they hold."),
                    OutcomeRoll(0.45, -2, 0, None, None, "The heal was promised away. You end worse."),
                ),
            ),
            MechanicOption(
                label="Refuse to act at all",
                flavor="Wait it out.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,       "The promise lapses. You take a small tax."),
                    OutcomeRoll(0.40, -2, 0, "player", None,   "She extends it. You're locked a round."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),

    "terrorblade_metamorphosis": BossMechanic(
        id="terrorblade_metamorphosis",
        archetype="charge_telegraph",
        trigger_round=3,
        prompt_title="the Sundered Prince enters his demon form",
        prompt_description="His wings unfurl. His next three swings will be ranged and devastating.",
        options=(
            MechanicOption(
                label="Close the distance immediately",
                flavor="Get inside his reach.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -2, None, None,    "Inside his range, his power drops. You trade well."),
                    OutcomeRoll(0.55, -3,  0, None, "bleed", "He swats you before you reach him."),
                ),
            ),
            MechanicOption(
                label="Hide behind cover",
                flavor="Wait out the duration.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,       "Two bolts miss. One scrapes the rock."),
                    OutcomeRoll(0.40, -3, 0, "player", None,   "The bolts punch through cover. You curl up."),
                ),
            ),
            MechanicOption(
                label="Counter with your own pick throw",
                flavor="Range vs range.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -3, None, None,   "Your throw catches him mid-bolt. Big dent."),
                    OutcomeRoll(0.70, -3,  0, None, None,   "He out-ranges you effortlessly."),
                ),
            ),
        ),
        safe_option_idx=1,
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
