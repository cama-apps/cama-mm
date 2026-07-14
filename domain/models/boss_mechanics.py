"""Mid-fight reactive mechanics for /dig boss duels.

A ``BossMechanic`` represents a single "moment" inside a boss fight where the
player is forced to make a reactive choice. Each boss has a ``mechanic_pool``
of compatible mechanic ids on its ``BossDef``; exactly one is rolled per fight and
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
# Regular bosses expose three compatible mechanics and pinnacle phases expose
# two. The compact helper below gives added variety mechanics the same bounded
# risk profile while their prompts and choices remain boss-specific.
#
# Shape conventions (documented for content authors):
#   - Exactly 3 options per mechanic
#   - Option probabilities for a single option's outcome_rolls must sum to 1.0
#   - trigger_round is typically between 2-6 (fights last <=20 rounds)
#   - narrative strings should be one sentence, present tense, <=100 chars
#   - safe_option_idx is the "don't do anything crazy" button — lowest variance.


def _variety_mechanic(
    *,
    mechanic_id: str,
    archetype: str,
    trigger_round: int,
    title: str,
    description: str,
    labels: tuple[str, str, str],
    flavors: tuple[str, str, str],
    failure_status: str,
) -> BossMechanic:
    """Build a themed mechanic within the established damage/control envelope."""
    return BossMechanic(
        id=mechanic_id,
        archetype=archetype,
        trigger_round=trigger_round,
        prompt_title=title,
        prompt_description=description,
        options=(
            MechanicOption(
                label=labels[0],
                flavor=flavors[0],
                outcome_rolls=(
                    OutcomeRoll(
                        0.75, -1, 0, None, None,
                        f"{flavors[0]} You hold steady.",
                    ),
                    OutcomeRoll(
                        0.25, -2, 0, None, None,
                        f"{flavors[0]} The timing costs you.",
                    ),
                ),
            ),
            MechanicOption(
                label=labels[1],
                flavor=flavors[1],
                outcome_rolls=(
                    OutcomeRoll(
                        0.55, -1, -2, None, "reveal",
                        f"{flavors[1]} The counter opens a weakness.",
                    ),
                    OutcomeRoll(
                        0.45, -2, 0, None, None,
                        f"{flavors[1]} The counter comes a beat late.",
                    ),
                ),
            ),
            MechanicOption(
                label=labels[2],
                flavor=flavors[2],
                outcome_rolls=(
                    OutcomeRoll(
                        0.35, 0, -3, None, None,
                        f"{flavors[2]} The gamble lands cleanly.",
                    ),
                    OutcomeRoll(
                        0.65, -3, 0, None, failure_status,
                        f"{flavors[2]} The gamble turns against you.",
                    ),
                ),
            ),
        ),
        safe_option_idx=0,
    )


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

    # ================================================================
    # PINNACLE — Forgotten King
    # ================================================================
    "king_decree": BossMechanic(
        id="king_decree",
        archetype="court_protocol",
        trigger_round=3,
        prompt_title="The King issues a Decree",
        prompt_description="A bony hand rises. \"Kneel, or be unmade.\"",
        options=(
            MechanicOption(
                label="Kneel — perform the gesture",
                flavor="You drop to one knee. The crown approves.",
                outcome_rolls=(
                    OutcomeRoll(0.70, 0, -1, None, None,         "He nods. Etiquette satisfied. You strike a leg cleanly."),
                    OutcomeRoll(0.30, -1, 0, None, "bleed",      "He smiles, then kicks you in the ribs. Tradition."),
                ),
            ),
            MechanicOption(
                label="Strike at the crown",
                flavor="You vault forward, pick raised.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -2, -3, None, None,         "Glancing blow on the crown — he reels."),
                    OutcomeRoll(0.60, -3, 0, "player", "silence", "His scepter cracks across your jaw. The throne speaks."),
                ),
            ),
            MechanicOption(
                label="Defy verbally",
                flavor="\"I owe no king down here.\"",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -1, None, None,         "He laughs, surprised. A truce of sorts."),
                    OutcomeRoll(0.50, -2, 0, None, "bleed",       "His ring backhands you. The cut is shallow but deep enough."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "king_feast": BossMechanic(
        id="king_feast",
        archetype="hunger_offering",
        trigger_round=3,
        prompt_title="The Crowned Hunger demands tribute",
        prompt_description="His mouth is no longer a mouth. It is a question.",
        options=(
            MechanicOption(
                label="Offer a ration",
                flavor="You toss your last loaf into the maw.",
                outcome_rolls=(
                    OutcomeRoll(0.65, 0, -2, None, None,        "He chews, distracted. You land two clean strikes."),
                    OutcomeRoll(0.35, -1, -1, None, None,       "He swallows fast and lashes back. Trade."),
                ),
            ),
            MechanicOption(
                label="Strike the open throat",
                flavor="You aim at the vulnerable second.",
                outcome_rolls=(
                    OutcomeRoll(0.45, 0, -4, None, None,         "Pick through cartilage. He chokes on his own scream."),
                    OutcomeRoll(0.55, -3, 0, None, "bleed",      "The throat snaps shut on your wrist."),
                ),
            ),
            MechanicOption(
                label="Empty your pockets and run a step back",
                flavor="Distraction by surplus.",
                outcome_rolls=(
                    OutcomeRoll(0.75, -1, 0, None, None,        "He stoops to scoop coins. You buy a breath."),
                    OutcomeRoll(0.25, -2, -1, None, None,       "He sees through it but still bites a coin. You both bleed."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),
    "king_deathbed": BossMechanic(
        id="king_deathbed",
        archetype="last_words",
        trigger_round=3,
        prompt_title="The King speaks his last lesson",
        prompt_description="His voice frays. He has one truth left to give — or to take.",
        options=(
            MechanicOption(
                label="Listen — let him finish",
                flavor="You lower your weapon. He nods.",
                outcome_rolls=(
                    OutcomeRoll(0.55, 0, -2, None, "reveal",     "He whispers a weakness in his armor. The crown brightens, then dims."),
                    OutcomeRoll(0.45, -2, 0, None, None,         "His final word was a curse. You stagger."),
                ),
            ),
            MechanicOption(
                label="End it now",
                flavor="No last words. No last anything.",
                outcome_rolls=(
                    OutcomeRoll(0.50, 0, -3, None, None,          "Your strike lands clean. The court grows silent."),
                    OutcomeRoll(0.50, -3, 0, "player", None,      "He chose this end and prepared for it. Counter-strike."),
                ),
            ),
            MechanicOption(
                label="Match his stillness",
                flavor="You wait, watching the crown.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, -2, None, None,         "He moves first, but slowly. You catch him on the turn."),
                    OutcomeRoll(0.40, -2, -1, None, None,         "Both of you blink. Both of you bleed."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),

    # ================================================================
    # PINNACLE — Hollowforged
    # ================================================================
    "hollow_walls_close": BossMechanic(
        id="hollow_walls_close",
        archetype="environmental_squeeze",
        trigger_round=3,
        prompt_title="The walls inhale around you",
        prompt_description="The chamber narrows. Stone teeth grow from the ceiling.",
        options=(
            MechanicOption(
                label="Climb above the stone teeth",
                flavor="You scramble up the wall.",
                outcome_rolls=(
                    OutcomeRoll(0.65, 0, -2, None, None,         "From above you find a soft seam. Two clean strikes."),
                    OutcomeRoll(0.35, -2, 0, None, None,         "Your foothold gives. You drop hard."),
                ),
            ),
            MechanicOption(
                label="Brace and let it close",
                flavor="You wedge your pick into the wall.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -1, None, None,         "The squeeze stalls. Both of you stuck, both of you bleeding."),
                    OutcomeRoll(0.45, -3, 0, None, "bleed",       "The walls find your ribs. They keep going."),
                ),
            ),
            MechanicOption(
                label="Dig sideways through the wall",
                flavor="You commit to a third tunnel.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, -2, None, None,         "Sideways breakthrough. You strike from outside the squeeze."),
                    OutcomeRoll(0.30, -2, 0, None, None,          "The wall fights back. You take a face full of grit."),
                ),
            ),
        ),
        safe_option_idx=2,
    ),
    "hollow_shape_shift": BossMechanic(
        id="hollow_shape_shift",
        archetype="reform_predict",
        trigger_round=3,
        prompt_title="Hollowforged reshapes",
        prompt_description="The mineral body folds inward, then reassembles in a new outline.",
        options=(
            MechanicOption(
                label="Predict the new edge",
                flavor="You commit to where it will be.",
                outcome_rolls=(
                    OutcomeRoll(0.50, 0, -3, None, None,          "You called it. Pick lands where the wall finishes forming."),
                    OutcomeRoll(0.50, -2, 0, None, None,          "Wrong guess. The new edge meets your face."),
                ),
            ),
            MechanicOption(
                label="Wait it out",
                flavor="You hold ground and watch.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, -1, None, None,         "Slow trade. You see the new shape and chip what you can."),
                    OutcomeRoll(0.30, -2, 0, None, "bleed",       "It reshapes around your stillness. A spike grazes your side."),
                ),
            ),
            MechanicOption(
                label="Charge through the shifting form",
                flavor="You commit before it solidifies.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, -4, None, None,           "You catch it half-formed. The body shudders apart."),
                    OutcomeRoll(0.60, -3, 0, "player", None,       "It hardens around you. You're inside the wall, briefly."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),
    "hollow_many_voices": BossMechanic(
        id="hollow_many_voices",
        archetype="distraction_chorus",
        trigger_round=3,
        prompt_title="A chorus rises from every wall",
        prompt_description="The mine speaks at once. Names. Insults. Promises.",
        options=(
            MechanicOption(
                label="Listen for the loudest voice",
                flavor="You hunt the source.",
                outcome_rolls=(
                    OutcomeRoll(0.55, 0, -3, None, "reveal",       "You find the speaker. Pick to vein."),
                    OutcomeRoll(0.45, -2, 0, None, "silence",      "The voices drown your thoughts. You hesitate."),
                ),
            ),
            MechanicOption(
                label="Ignore the chorus and swing",
                flavor="You attack the air at random.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -2, None, None,           "You hit something. Hard to say what."),
                    OutcomeRoll(0.55, -2, 0, None, "bleed",         "The voices were a feint. The real strike came from behind."),
                ),
            ),
            MechanicOption(
                label="Sing back",
                flavor="You match their pitch with a curse of your own.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -2, None, None,           "The chorus stutters. You step in and chip the wall."),
                    OutcomeRoll(0.50, -2, -1, None, None,           "Both of you mid-song, both of you bleeding."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # PINNACLE — The First Digger
    # ================================================================
    "digger_pickaxe_duel": BossMechanic(
        id="digger_pickaxe_duel",
        archetype="weapon_duel",
        trigger_round=3,
        prompt_title="The First Digger raises his pickaxe",
        prompt_description="Two diggers, one tunnel. Only one pick will leave whole.",
        options=(
            MechanicOption(
                label="Parry his swing",
                flavor="You meet pick with pick.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, -1, None, None,            "Sparks. Both of you push back. Even trade."),
                    OutcomeRoll(0.40, -2, 0, None, None,             "His pick has more weight than you expected."),
                ),
            ),
            MechanicOption(
                label="Sidestep and drive in",
                flavor="You commit to advance.",
                outcome_rolls=(
                    OutcomeRoll(0.45, 0, -3, None, None,             "You slip past — pick to the chest."),
                    OutcomeRoll(0.55, -3, 0, "player", None,         "He pivots faster than you. Your ribs find his haft."),
                ),
            ),
            MechanicOption(
                label="Lock haft to haft, push him into the wall",
                flavor="A wrestler's move underground.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, None,            "You out-leverage him. He cracks against stone."),
                    OutcomeRoll(0.45, -2, -1, None, None,            "He's been at this for a century. You both stagger."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "digger_phasing": BossMechanic(
        id="digger_phasing",
        archetype="phase_chase",
        trigger_round=3,
        prompt_title="He flickers between solid and not",
        prompt_description="Half a step out of phase, the Digger blinks through your strikes.",
        options=(
            MechanicOption(
                label="Anticipate his solid moment",
                flavor="You hold and time the swing.",
                outcome_rolls=(
                    OutcomeRoll(0.55, 0, -3, None, None,             "Caught mid-phase. Pick passes through skin first, then air."),
                    OutcomeRoll(0.45, -2, 0, None, None,             "Wrong moment. You swing through nothing while he was already behind."),
                ),
            ),
            MechanicOption(
                label="Trap him with terrain",
                flavor="You back into a corner he must turn through.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, -2, None, "reveal",        "He has to commit to a direction. You have it covered."),
                    OutcomeRoll(0.35, -2, 0, None, "silence",        "He passes through stone. He was never going to commit."),
                ),
            ),
            MechanicOption(
                label="Phase yourself — match his rhythm",
                flavor="You step out of phase too. Maybe.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, -4, None, None,              "You both drop into the half-place. You hit harder there."),
                    OutcomeRoll(0.60, -3, 0, "player", "bleed",       "You weren't ready. He drags you back the wrong way."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),
    "digger_tunnel_collapse": BossMechanic(
        id="digger_tunnel_collapse",
        archetype="environmental_collapse",
        trigger_round=3,
        prompt_title="The tunnel begins to fold",
        prompt_description="\"I am the tunnel,\" he says. The walls listen.",
        options=(
            MechanicOption(
                label="Dig out — straight up",
                flavor="You commit to the surface.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None,             "You break a vent and stagger. The collapse passes around you."),
                    OutcomeRoll(0.45, -3, 0, None, "bleed",          "The vent caves. Stone wedges your shoulder."),
                ),
            ),
            MechanicOption(
                label="Flatten — let it pass over",
                flavor="You drop and pray.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, -1, None, None,            "Most of it passes. The last block clips your hip."),
                    OutcomeRoll(0.35, -3, 0, None, None,             "You weren't flat enough. Something heavy finds you."),
                ),
            ),
            MechanicOption(
                label="Push deeper — into him",
                flavor="If he is the tunnel, you can attack the tunnel.",
                outcome_rolls=(
                    OutcomeRoll(0.45, 0, -4, None, None,              "You strike the floor — and he flinches."),
                    OutcomeRoll(0.55, -3, -1, None, None,             "The floor swallows you both. Painful for both of you."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    # ================================================================
    # TIER 150 — late-prestige additions
    # ================================================================
    # Prestige-2 entries share this section with the deeper prestige-gated bosses.
    "aegis_reclaim": BossMechanic(
        id="aegis_reclaim",
        archetype="second_life",
        trigger_round=4,
        prompt_title="The aegis lights with a borrowed heartbeat",
        prompt_description="The Warden lifts the shield. The next mistake wants to happen twice.",
        options=(
            MechanicOption(
                label="Crack the shield edge",
                flavor="You swing for the weakest glow.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -3, None, "reveal", "The shield cracks; his return flickers."),
                    OutcomeRoll(0.45, -2, 0, None, None, "The shield rolls the blow aside and numbs your arm."),
                ),
            ),
            MechanicOption(
                label="Wait out the glow",
                flavor="You hold distance and count breaths.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, 0, None, None, "The glow fades before it can spend itself."),
                    OutcomeRoll(0.30, -2, 0, "player", None, "You waited too long; he retakes the tempo."),
                ),
            ),
            MechanicOption(
                label="Bait the return",
                flavor="You offer a false opening.",
                outcome_rolls=(
                    OutcomeRoll(0.35, 0, -4, None, None, "He commits to the return; you punish the second step."),
                    OutcomeRoll(0.65, -3, 0, None, "bleed", "He reads the bait and shieldsmashes through it."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),
    "heartspire_intent": BossMechanic(
        id="heartspire_intent",
        archetype="telegraphed_intent",
        trigger_round=5,
        prompt_title="The Heartspire shows its intent",
        prompt_description="A red line, a shielded line, and a lie pulse across the stone.",
        options=(
            MechanicOption(
                label="Block the red line",
                flavor="You trust the obvious threat.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, 0, None, None, "The line strikes your guard and breaks there."),
                    OutcomeRoll(0.35, -2, 0, None, "silence", "The obvious threat was only half of it."),
                ),
            ),
            MechanicOption(
                label="Strike the shielded line",
                flavor="You attack the defense.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -3, None, "reveal", "The shielded line opens. The heart is briefly exposed."),
                    OutcomeRoll(0.55, -3, 0, None, None, "You hit the guard and the guard hits back."),
                ),
            ),
            MechanicOption(
                label="Ignore the lie",
                flavor="You choose the line that is not pulsing.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, -4, None, None, "The hidden line was the real path. Your pick finds it."),
                    OutcomeRoll(0.60, -3, 0, "player", "bleed", "The lie was bait. The spire was counting on you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "emberwright_overclock": BossMechanic(
        id="emberwright_overclock",
        archetype="forge_overheat",
        trigger_round=6,
        prompt_title="The ember engine overclocks",
        prompt_description="Gears glow white. The forge starts eating its own light.",
        options=(
            MechanicOption(
                label="Vent the engine",
                flavor="You break open a pressure valve.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, -2, None, None, "Steam and sparks vent upward. The engine stutters."),
                    OutcomeRoll(0.40, -2, 0, None, "burn", "The valve vents through you first."),
                ),
            ),
            MechanicOption(
                label="Kick slag into the gears",
                flavor="Jam the machine with its own waste.",
                outcome_rolls=(
                    OutcomeRoll(0.50, -1, -3, None, "reveal", "The gears choke on slag. The forge is exposed."),
                    OutcomeRoll(0.50, -3, 0, None, None, "The gears spit the slag back, still molten."),
                ),
            ),
            MechanicOption(
                label="Ride the heat wave",
                flavor="Use the blast to close distance.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -2, -4, None, None, "You surf the blast straight into a brutal strike."),
                    OutcomeRoll(0.65, -4, 0, "player", "burn", "The wave lifts you, then the ceiling returns you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    "xalatath_void_pull": BossMechanic(
        id="xalatath_void_pull",
        archetype="hook_pull",
        trigger_round=4,
        prompt_title="A seam opens. It pulls.",
        prompt_description="The dark below your feet has a direction. Toward her.",
        options=(
            MechanicOption(
                label="Anchor to the wall",
                flavor="You drive your pick into stone.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, 0, None, None,             "You hold. Something brushes past your ankle."),
                    OutcomeRoll(0.35, -2, 0, None, "silence",        "The pick slips. You hear yourself stop existing for a moment."),
                ),
            ),
            MechanicOption(
                label="Let the pull take you",
                flavor="Use the momentum. Strike on the way in.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -3, None, None,            "You ride the seam. Your pick finds her ribs."),
                    OutcomeRoll(0.55, -3, 0, None, "bleed",          "She closes the seam around you. Something tears."),
                ),
            ),
            MechanicOption(
                label="Speak her name back",
                flavor="If she heard you, she might hesitate.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, -2, None, "reveal",         "She pauses. The seam slackens. Her shape thins."),
                    OutcomeRoll(0.60, -2, 0, None, "silence",        "You said it wrong. The name takes more than you offered."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "xalatath_whisper_madness": BossMechanic(
        id="xalatath_whisper_madness",
        archetype="reality_warp",
        trigger_round=5,
        prompt_title="A whisper, not in your ear",
        prompt_description="It's already in your head. It's been there a while.",
        options=(
            MechanicOption(
                label="Listen carefully",
                flavor="Hear it out.",
                outcome_rolls=(
                    OutcomeRoll(0.45, 0, -2, None, "reveal",         "You catch a syllable that wasn't meant for you. She flinches."),
                    OutcomeRoll(0.55, -2, 0, None, "silence",        "The whisper folds. You can't recall a word now."),
                ),
            ),
            MechanicOption(
                label="Cover your ears and hum",
                flavor="Drown her out with noise.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,             "The hum holds. The whisper retreats — for now."),
                    OutcomeRoll(0.40, -2, 0, "player", "silence",    "She whispered louder. The hum cracks. So does your nerve."),
                ),
            ),
            MechanicOption(
                label="Repeat the whisper aloud",
                flavor="Give it back to her.",
                outcome_rolls=(
                    OutcomeRoll(0.35, -1, -3, None, None,            "She didn't expect that. The whisper recoils into her."),
                    OutcomeRoll(0.65, -3, 0, None, "silence",        "You said it. You shouldn't have. Something rearranges in your chest."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    # ================================================================
    # TIER 200 — late-prestige additions
    # ================================================================
    "lilith_blood_nova": BossMechanic(
        id="lilith_blood_nova",
        archetype="channel_aoe",
        trigger_round=3,
        prompt_title="Blood lifts off the floor",
        prompt_description="It hangs around her like a held breath.",
        options=(
            MechanicOption(
                label="Take cover behind a column",
                flavor="Let the wave pass.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, 0, None, None,             "The column splatters. Most of you stays dry."),
                    OutcomeRoll(0.30, -2, 0, None, "bleed",          "It found a seam in the stone. It found yours too."),
                ),
            ),
            MechanicOption(
                label="Dive through the center",
                flavor="Take the wave at her.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -2, -3, None, None,            "You break through. She's too close to escape your pick."),
                    OutcomeRoll(0.60, -3, 0, None, "bleed",          "The wave is denser than it looks. You stagger out of it red."),
                ),
            ),
            MechanicOption(
                label="Stand still and let it find you",
                flavor="Bleed back what's hers.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -2, None, "reveal",        "She underestimated you. The blood reads your shape and reveals hers."),
                    OutcomeRoll(0.70, -3, 0, "player", "bleed",      "The blood remembers you. Cycle bleed. You miss a beat."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "lilith_wing_descent": BossMechanic(
        id="lilith_wing_descent",
        archetype="aerial_slam",
        trigger_round=5,
        prompt_title="She rises on two black wings",
        prompt_description="Higher than the ceiling should allow. She is going to come down.",
        options=(
            MechanicOption(
                label="Roll under the descent",
                flavor="Time the dodge.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, None,            "You roll. The crater misses. You hit her ankle going past."),
                    OutcomeRoll(0.45, -3, 0, None, "bleed",          "Your timing was a half-beat off. The wing-edge clips you."),
                ),
            ),
            MechanicOption(
                label="Brace for the impact",
                flavor="Plant and weather it.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -2, 0, None, None,             "The shockwave passes through you. You stay upright."),
                    OutcomeRoll(0.35, -4, 0, None, None,             "She landed harder than the floor allowed. So did you."),
                ),
            ),
            MechanicOption(
                label="Throw your pick at her descent",
                flavor="Interrupt mid-air.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -4, None, "reveal",         "The pick threads between her wings. She drops awkwardly."),
                    OutcomeRoll(0.70, -3, 0, None, None,             "She caught it. Now you are unarmed, briefly. She is not."),
                ),
            ),
        ),
        safe_option_idx=1,
    ),

    # ================================================================
    # TIER 275 — late-prestige additions
    # ================================================================
    "underlord_pit_pull": BossMechanic(
        id="underlord_pit_pull",
        archetype="hook_pull",
        trigger_round=4,
        prompt_title="The floor opens beneath you",
        prompt_description="A shaft drops into nothing. He's standing at the edge, waiting.",
        options=(
            MechanicOption(
                label="Catch the lip and haul up",
                flavor="Don't fall. Don't fall.",
                outcome_rolls=(
                    OutcomeRoll(0.65, -1, 0, None, None,             "You catch the rim. Knuckles burn. You climb."),
                    OutcomeRoll(0.35, -3, 0, None, "bleed",          "The rim gives. You eat stone on the way back up."),
                ),
            ),
            MechanicOption(
                label="Drop and strike on the way down",
                flavor="If you're going down, take a piece of him.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -2, -4, None, None,            "You drop into him pick-first. He grunts. He didn't expect that."),
                    OutcomeRoll(0.60, -4, 0, None, None,             "He stepped aside. You hit the bottom alone. The bottom is rude."),
                ),
            ),
            MechanicOption(
                label="Anchor your pick crosswise",
                flavor="Wedge it across the shaft. Stop the fall.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, "reveal",         "The pick wedges. You hang there. He has to come to you."),
                    OutcomeRoll(0.45, -2, 0, "player", None,         "The pick shears. You're below him now. He looks down at you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "underlord_firestorm": BossMechanic(
        id="underlord_firestorm",
        archetype="channel_aoe",
        trigger_round=6,
        prompt_title="He calls down a firestorm",
        prompt_description="The ceiling forgets it's stone for a moment.",
        options=(
            MechanicOption(
                label="Shelter under an outcrop",
                flavor="Wait it out. Take the cooldown.",
                outcome_rolls=(
                    OutcomeRoll(0.70, -1, 0, None, None,             "The rock holds. Heat washes past. You stay dry."),
                    OutcomeRoll(0.30, -2, 0, None, "burn",           "An ember finds the gap. Smolders into your sleeve."),
                ),
            ),
            MechanicOption(
                label="Sprint through the storm at him",
                flavor="Close the distance under fire.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -2, -3, None, None,            "You come out the other side smoking and swinging. He didn't move in time."),
                    OutcomeRoll(0.60, -3, 0, None, "burn",           "The storm got most of you. The pick got air."),
                ),
            ),
            MechanicOption(
                label="Use the smoke to flank low",
                flavor="Get under him.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -3, None, "reveal",        "You come up inside his guard. He takes a clean hit."),
                    OutcomeRoll(0.55, -3, 0, None, "burn",           "He saw the smoke move. The pit lord knows smoke."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 150 — prestige-4 (The Blightcoil)
    # ================================================================
    "blightcoil_wards": BossMechanic(
        id="blightcoil_wards",
        archetype="summon_swarm",
        trigger_round=3,
        prompt_title="The Blightcoil spits a ring of plague-wards",
        prompt_description="Three twitching pods hiss open around you, breathing spores.",
        options=(
            MechanicOption(
                label="Smash the nearest ward",
                flavor="Crush it before it ripens.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,             "You stomp one flat; the others keep hissing."),
                    OutcomeRoll(0.40, -2, 0, None, "bleed",          "Spores burst up your arm as it pops."),
                ),
            ),
            MechanicOption(
                label="Push through to the coil",
                flavor="Ignore the pods, reach the source.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -2, -2, None, None,            "You wade through the haze and land a hit on the coil."),
                    OutcomeRoll(0.55, -3, 0, None, "bleed",          "The wards drain you on the way in."),
                ),
            ),
            MechanicOption(
                label="Torch the whole ring",
                flavor="Burn the garden down.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -3, None, None,             "The pods catch and chain-burst into the Blightcoil."),
                    OutcomeRoll(0.70, -3, 0, None, "burn",           "Spore-gas is flammable. So, briefly, were you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "blightcoil_nova": BossMechanic(
        id="blightcoil_nova",
        archetype="channel_aoe",
        trigger_round=4,
        prompt_title="The Blightcoil swells with a venom nova",
        prompt_description="It draws a long breath. The chamber goes green at the edges.",
        options=(
            MechanicOption(
                label="Hold your breath and back off",
                flavor="Retreat to clean air.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,             "You reach clean air; a little gets in anyway."),
                    OutcomeRoll(0.40, -2, 0, None, "bleed",          "You inhale at exactly the wrong moment."),
                ),
            ),
            MechanicOption(
                label="Charge before it releases",
                flavor="Cut the breath short.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -3, None, None,            "A pick to the throat cuts the nova off at the source."),
                    OutcomeRoll(0.60, -3, 0, None, "bleed",          "It exhales in your face. The garden takes root."),
                ),
            ),
            MechanicOption(
                label="Lance a pod to vent the gas",
                flavor="Give the nova somewhere else to go.",
                outcome_rolls=(
                    OutcomeRoll(0.35, 0, -2, None, "reveal",         "The nova vents sideways through the pod and exposes the coil."),
                    OutcomeRoll(0.65, -2, 0, None, "bleed",          "Wrong pod. The gas funnels straight to you."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 200 — prestige-4 (The Rimebound King)
    # ================================================================
    "rimebound_harvest": BossMechanic(
        id="rimebound_harvest",
        archetype="channel_big_hit",
        trigger_round=3,
        prompt_title="The Rimebound King raises the runeblade to reap",
        prompt_description="Frost crawls up the blade. It is hungry for something warm.",
        options=(
            MechanicOption(
                label="Keep your distance",
                flavor="Stay out of the arc.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,             "The reap falls short; only the cold reaches your fingers."),
                    OutcomeRoll(0.40, -2, 0, None, "frostbite",      "The edge of the arc catches you; the chill sinks in."),
                ),
            ),
            MechanicOption(
                label="Step inside the swing",
                flavor="Too close for the blade.",
                outcome_rolls=(
                    OutcomeRoll(0.45, -1, -3, None, None,            "Inside the arc, you hammer the breastplate twice."),
                    OutcomeRoll(0.55, -2, 0, None, "frostbite",      "He shortens his grip. The blade still finds you."),
                ),
            ),
            MechanicOption(
                label="Catch the blade on your pick",
                flavor="Bind it and crack his guard.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -3, None, None,             "Sparks fly; you wrench the runeblade wide and crack his guard."),
                    OutcomeRoll(0.70, -3, 1, None, "frostbite",      "The blade drinks through the pick. He stands a little taller."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "rimebound_raise": BossMechanic(
        id="rimebound_raise",
        archetype="summon_swarm",
        trigger_round=4,
        prompt_title="The Rimebound King raises a frozen thrall",
        prompt_description="A corpse of ice claws its way up from the floor between you.",
        options=(
            MechanicOption(
                label="Shatter the thrall first",
                flavor="Put it down before it stands.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, 0, None, None,             "You smash it to slush before it finds its feet."),
                    OutcomeRoll(0.45, -2, 0, None, None,             "It grabs you as it breaks; cold hands, slow to let go."),
                ),
            ),
            MechanicOption(
                label="Ignore it, press the King",
                flavor="The crown is the real target.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -2, -3, None, None,            "You leave the thrall behind and bury your pick in the King."),
                    OutcomeRoll(0.60, -3, 0, None, "frostbite",      "The thrall hamstrings you from behind."),
                ),
            ),
            MechanicOption(
                label="Turn the thrall against him",
                flavor="Shove it into the throne.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -3, None, "reveal",         "You drive the thrall into the King; both stagger, his guard opens."),
                    OutcomeRoll(0.70, -2, 0, "player", None,         "The thrall obeys only the crown. It pins you for the round."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # TIER 275 — prestige-4 (The Spineback)
    # ================================================================
    "spineback_regrowth": BossMechanic(
        id="spineback_regrowth",
        archetype="bind_debuff",
        trigger_round=3,
        prompt_title="The Spineback's black spines harden over",
        prompt_description="The wounds you opened crust with new growth. It is healing in front of you.",
        options=(
            MechanicOption(
                label="Break the fresh white spines",
                flavor="Hit the soft new growth.",
                outcome_rolls=(
                    OutcomeRoll(0.55, -1, -2, None, "reveal",        "You snap the soft growth; it can't harden there. It flinches."),
                    OutcomeRoll(0.45, -2, 0, None, None,             "Wrong spine — the hardened plates turn your blow."),
                ),
            ),
            MechanicOption(
                label="Hammer the black plates",
                flavor="Chip the hardened armor.",
                outcome_rolls=(
                    OutcomeRoll(0.30, -1, -1, None, None,            "You chip a black plate; slow going, but it holds still for it."),
                    OutcomeRoll(0.70, -3, 0, None, "bleed",          "The plates are stone now. Your pick skips into a spine."),
                ),
            ),
            MechanicOption(
                label="Strike before it finishes",
                flavor="Catch it mid-regrowth.",
                outcome_rolls=(
                    OutcomeRoll(0.40, 0, -3, None, None,             "You catch it mid-knit; the new growth tears away with the old."),
                    OutcomeRoll(0.60, -3, 0, None, "bleed",          "It finished first. The fresh spines are already sharp."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),
    "spineback_divebomb": BossMechanic(
        id="spineback_divebomb",
        archetype="charge_telegraph",
        trigger_round=5,
        prompt_title="The Spineback launches off the wall",
        prompt_description="It hits the ceiling and folds, spines-first, into a dive straight down at you.",
        options=(
            MechanicOption(
                label="Dive aside at the last instant",
                flavor="Wait for the commit.",
                outcome_rolls=(
                    OutcomeRoll(0.60, -1, 0, None, None,             "You throw yourself clear; the impact craters where you stood."),
                    OutcomeRoll(0.40, -2, 0, None, None,             "A trailing spine rakes your back as it lands."),
                ),
            ),
            MechanicOption(
                label="Meet it with a raised pick",
                flavor="Set the pick like a spear.",
                outcome_rolls=(
                    OutcomeRoll(0.30, 0, -3, None, None,             "It impales itself on the drop. The sound is tremendous."),
                    OutcomeRoll(0.70, -3, 0, "player", None,         "It is far heavier than you. You're driven into the floor."),
                ),
            ),
            MechanicOption(
                label="Roll under to the soft belly",
                flavor="Get beneath the spines.",
                outcome_rolls=(
                    OutcomeRoll(0.40, -1, -3, None, "reveal",        "You slide under the spines and open its underside."),
                    OutcomeRoll(0.60, -3, 0, None, "bleed",          "The belly is armored too. The spines are not kind."),
                ),
            ),
        ),
        safe_option_idx=0,
    ),

    # ================================================================
    # VARIETY EXPANSION — REGULAR BOSSES
    # ================================================================
    "grothak_bedrock_bellow": _variety_mechanic(
        mechanic_id="grothak_bedrock_bellow",
        archetype="channel_aoe",
        trigger_round=2,
        title="Grothak bellows into the bedrock",
        description="The stone answers him, shaking loose in widening rings.",
        labels=("Drop below the tremor", "Strike his ribs", "Bellow back"),
        flavors=("You flatten against the floor.", "You step inside his breath.", "You answer stone with spite."),
        failure_status="bleed",
    ),
    "crystalia_mirror_maze": _variety_mechanic(
        mechanic_id="crystalia_mirror_maze",
        archetype="reality_warp",
        trigger_round=2,
        title="Crystalia folds the chamber into mirrors",
        description="Every reflection raises a pick half a heartbeat before you do.",
        labels=("Watch the floor", "Crack the dull mirror", "Charge your reflection"),
        flavors=("You follow the one shadow that stays true.", "You find the facet without light.", "You sprint at your own raised pick."),
        failure_status="frostbite",
    ),
    "magmus_lava_tide": _variety_mechanic(
        mechanic_id="magmus_lava_tide",
        archetype="dot_debuff",
        trigger_round=3,
        title="Magmus calls the lava uphill",
        description="A molten tide climbs the cavern toward your boots.",
        labels=("Climb the basalt shelf", "Cut a cooling trench", "Vault through the crest"),
        flavors=("You scramble above the orange wash.", "You split the black crust ahead of it.", "You leap where the lava curls highest."),
        failure_status="burn",
    ),
    "voidwarden_gravity_well": _variety_mechanic(
        mechanic_id="voidwarden_gravity_well",
        archetype="reality_warp",
        trigger_round=5,
        title="The Void Warden removes the floor's permission",
        description="Dust, stone, and your own limbs begin falling sideways.",
        labels=("Anchor to a seam", "Cut the dark center", "Let the pull sling you"),
        flavors=("You hook your pick into stubborn stone.", "You drag the blade across the well's edge.", "You surrender to the impossible fall."),
        failure_status="silence",
    ),
    "sporeling_bloom": _variety_mechanic(
        mechanic_id="sporeling_bloom",
        archetype="summon_swarm",
        trigger_round=5,
        title="The Sovereign blooms all at once",
        description="A crown of pale caps opens and sheds hungry motes.",
        labels=("Cover your mouth", "Cull the lowest caps", "Breathe it in and rush"),
        flavors=("You wrap your sleeve tight.", "You sweep the fresh blooms away.", "You charge through the sweet fog."),
        failure_status="bleed",
    ),
    "chronofrost_time_shard": _variety_mechanic(
        mechanic_id="chronofrost_time_shard",
        archetype="rewind",
        trigger_round=3,
        title="Chronofrost breaks off a second of time",
        description="The blue shard shows your next wound already frozen inside it.",
        labels=("Wait out the vision", "Shatter the shard", "Step into your future"),
        flavors=("You refuse the moment it offers.", "You strike the frozen second sideways.", "You meet the wound before it exists."),
        failure_status="frostbite",
    ),
    "nameless_false_floor": _variety_mechanic(
        mechanic_id="nameless_false_floor",
        archetype="reality_warp",
        trigger_round=2,
        title="The floor forgets that it is stone",
        description="Your boots sink into a depth with no bottom and no name.",
        labels=("Hold perfectly still", "Name the nearest ledge", "Dive beneath the illusion"),
        flavors=("You give the lie nothing to use.", "You force one edge back into being.", "You descend into the missing place."),
        failure_status="silence",
    ),
    "pudge_dismember": _variety_mechanic(
        mechanic_id="pudge_dismember",
        archetype="channel_big_hit",
        trigger_round=5,
        title="Pudge reaches for a butcher's grip",
        description="The cleaver lowers; the other hand comes in fast.",
        labels=("Keep outside his reach", "Jam the cleaver arm", "Duck under the grab"),
        flavors=("You give the butcher no clean hold.", "You wedge your pick against his elbow.", "You slip beneath both enormous arms."),
        failure_status="bleed",
    ),
    "ogre_bloodlust": _variety_mechanic(
        mechanic_id="ogre_bloodlust",
        archetype="charge_telegraph",
        trigger_round=2,
        title="Ogre Magi argues himself into a frenzy",
        description="Both heads agree on one thing: hit faster, hit harder.",
        labels=("Circle until they disagree", "Interrupt the loud head", "Match the frenzy"),
        flavors=("You wait for the argument to restart.", "You ring the head doing the counting.", "You turn the chamber into a brawl."),
        failure_status="burn",
    ),
    "cm_crystal_nova": _variety_mechanic(
        mechanic_id="cm_crystal_nova",
        archetype="channel_aoe",
        trigger_round=3,
        title="Crystal Maiden gathers a silent nova",
        description="Frost races outward beneath the dust in a perfect circle.",
        labels=("Step beyond the rim", "Break the ice at her feet", "Cross the center"),
        flavors=("You retreat ahead of the pale ring.", "You fracture the nova before it spreads.", "You run straight through the coldest point."),
        failure_status="frostbite",
    ),
    "tusk_ice_shards": _variety_mechanic(
        mechanic_id="tusk_ice_shards",
        archetype="bind_debuff",
        trigger_round=2,
        title="Tusk walls the tunnel with ice shards",
        description="Jagged blue teeth erupt behind and beside you.",
        labels=("Guard the open lane", "Chip the base", "Vault the forming wall"),
        flavors=("You keep the last escape lane clear.", "You break the weakest shard at its root.", "You leap before the teeth meet."),
        failure_status="frostbite",
    ),
    "lina_light_strike": _variety_mechanic(
        mechanic_id="lina_light_strike",
        archetype="channel_big_hit",
        trigger_round=2,
        title="Lina marks the ground with white fire",
        description="A bright circle tightens around your boots.",
        labels=("Walk with the edge", "Throw dust at Lina", "Hold and strike upward"),
        flavors=("You pace the shrinking rim.", "You spoil her sightline with ash.", "You trust the instant before ignition."),
        failure_status="burn",
    ),
    "doom_infernal_blade": _variety_mechanic(
        mechanic_id="doom_infernal_blade",
        archetype="mark_delayed",
        trigger_round=2,
        title="Doom's blade burns from the inside",
        description="The sword leaves a red afterimage aimed through your guard.",
        labels=("Give ground", "Catch the flat", "Trade through the flame"),
        flavors=("You yield before the blade commits.", "You turn the broad side with your pick.", "You step into the burning arc."),
        failure_status="burn",
    ),
    "spectre_dispersion": _variety_mechanic(
        mechanic_id="spectre_dispersion",
        archetype="gamble",
        trigger_round=4,
        title="Spectre turns your force back on you",
        description="Every hard swing returns as a violet echo.",
        labels=("Use shallow cuts", "Strike between echoes", "Commit everything"),
        flavors=("You shorten each careful swing.", "You attack in the reflection's blind beat.", "You bet the echo breaks first."),
        failure_status="bleed",
    ),
    "void_spirit_resonant_pulse": _variety_mechanic(
        mechanic_id="void_spirit_resonant_pulse",
        archetype="channel_aoe",
        trigger_round=5,
        title="Void Spirit rings the aether like a bell",
        description="A violet pulse swells beneath a shield of impossible angles.",
        labels=("Ride behind the pulse", "Crack the shield seam", "Meet it head-on"),
        flavors=("You move in the pulse's quiet wake.", "You find the shield's unfinished edge.", "You drive your pick into the violet wave."),
        failure_status="silence",
    ),
    "treant_living_armor": _variety_mechanic(
        mechanic_id="treant_living_armor",
        archetype="bind_debuff",
        trigger_round=5,
        title="Treant's bark knits over every wound",
        description="Fresh rings of wood close around the cuts you made.",
        labels=("Peel the green bark", "Cut the binding vines", "Split the oldest ring"),
        flavors=("You strip away the soft new layer.", "You sever the vines feeding the repair.", "You swing for the heartwood beneath."),
        failure_status="bleed",
    ),
    "broodmother_silken_snare": _variety_mechanic(
        mechanic_id="broodmother_silken_snare",
        archetype="bind_debuff",
        trigger_round=2,
        title="Broodmother snaps a silk line taut",
        description="The tunnel becomes a web with you at its center.",
        labels=("Freeze before it tightens", "Cut the anchor strand", "Pull her into the web"),
        flavors=("You stop the silk from sawing deeper.", "You find the strand carrying all the tension.", "You wrap the line and haul back."),
        failure_status="bleed",
    ),
    "faceless_void_time_dilation": _variety_mechanic(
        mechanic_id="faceless_void_time_dilation",
        archetype="time_skip",
        trigger_round=2,
        title="Faceless Void stretches the instant between swings",
        description="Your muscles wait while his mace keeps moving.",
        labels=("Release your grip", "Move before deciding", "Force the delayed swing"),
        flavors=("You stop fighting the stolen second.", "You act on instinct before time catches up.", "You drag the trapped motion through."),
        failure_status="silence",
    ),
    "weaver_geminate_strike": _variety_mechanic(
        mechanic_id="weaver_geminate_strike",
        archetype="charge_telegraph",
        trigger_round=4,
        title="Weaver's first strike pulls a second behind it",
        description="The echoing claw arrives from an angle that does not exist yet.",
        labels=("Guard the echo", "Punish the first claw", "Stand between both strikes"),
        flavors=("You ignore the feint and wait.", "You cut across the first lunge.", "You choose the narrowing space between."),
        failure_status="bleed",
    ),
    "oracle_purifying_flames": _variety_mechanic(
        mechanic_id="oracle_purifying_flames",
        archetype="gamble",
        trigger_round=2,
        title="Oracle offers a flame that wounds before it heals",
        description="The green fire asks whether you can survive its first answer.",
        labels=("Refuse the bargain", "Turn the flame on Oracle", "Take the full prophecy"),
        flavors=("You step outside the promised cure.", "You catch the green fire on your pick.", "You accept pain and whatever follows."),
        failure_status="burn",
    ),
    "terrorblade_reflection": _variety_mechanic(
        mechanic_id="terrorblade_reflection",
        archetype="reality_warp",
        trigger_round=4,
        title="Terrorblade pulls your reflection free",
        description="It knows your stance, your reach, and exactly where you hesitate.",
        labels=("Change your rhythm", "Attack Terrorblade", "Duel yourself"),
        flavors=("You become unfamiliar to your copy.", "You ignore the copy and cut at its source.", "You meet your own best swing."),
        failure_status="silence",
    ),
    "xalatath_blackout": _variety_mechanic(
        mechanic_id="xalatath_blackout",
        archetype="reality_warp",
        trigger_round=3,
        title="Xal'atath closes every source of light",
        description="Her whisper remains, moving where the chamber should be.",
        labels=("Count your breaths", "Strike toward the echo", "Answer the whisper"),
        flavors=("You make your own measure of the dark.", "You cut where the second echo overlaps.", "You speak into the voice behind your eyes."),
        failure_status="silence",
    ),
    "lilith_blood_tether": _variety_mechanic(
        mechanic_id="lilith_blood_tether",
        archetype="bind_debuff",
        trigger_round=4,
        title="Lilith knots a red tether between your hearts",
        description="Each beat drags you one step closer to her wings.",
        labels=("Slow your breathing", "Sever it on a heartbeat", "Pull her closer first"),
        flavors=("You quiet the rhythm feeding the cord.", "You cut as both hearts strike together.", "You wrap the tether around your wrist."),
        failure_status="bleed",
    ),
    "underlord_dark_rift": _variety_mechanic(
        mechanic_id="underlord_dark_rift",
        archetype="reality_warp",
        trigger_round=3,
        title="Underlord tears open a dark rift",
        description="A burning battlefield waits on the other side of the split.",
        labels=("Brace against the pull", "Collapse the rift edge", "Follow him through"),
        flavors=("You set both boots against the stone.", "You strike the seam holding two places together.", "You leap after the retreating silhouette."),
        failure_status="burn",
    ),
    "blightcoil_soul_lattice": _variety_mechanic(
        mechanic_id="blightcoil_soul_lattice",
        archetype="summon_swarm",
        trigger_round=5,
        title="The Blightcoil braids its wards into a soul lattice",
        description="Cold lines connect every skull and tighten around your shadow.",
        labels=("Stay between the lines", "Break the lowest skull", "Seize the lattice"),
        flavors=("You fold yourself into the one open angle.", "You knock the keystone ward loose.", "You grab the cold threads with both hands."),
        failure_status="silence",
    ),
    "rimebound_frozen_throne": _variety_mechanic(
        mechanic_id="rimebound_frozen_throne",
        archetype="bind_debuff",
        trigger_round=5,
        title="The Rimebound King raises a throne of ice",
        description="Every shard points outward as he settles into the crown.",
        labels=("Shelter behind a pillar", "Break the throne's foot", "Climb the front steps"),
        flavors=("You put thick ice between you and the crown.", "You hammer the weight-bearing corner.", "You charge straight up the frozen dais."),
        failure_status="frostbite",
    ),
    "spineback_quill_barrage": _variety_mechanic(
        mechanic_id="spineback_quill_barrage",
        archetype="summon_swarm",
        trigger_round=2,
        title="The Spineback fans every quill outward",
        description="Black needles tremble, each aimed at a different escape.",
        labels=("Hide behind the shed shell", "Strike before the release", "Thread the barrage"),
        flavors=("You drag a broken plate into cover.", "You hit the muscle bunching beneath the spines.", "You run through the first opening."),
        failure_status="bleed",
    ),
    "aegis_bulwark": _variety_mechanic(
        mechanic_id="aegis_bulwark",
        archetype="bind_debuff",
        trigger_round=2,
        title="The Aegis Warden locks the shield into bedrock",
        description="A golden wall divides the chamber and advances one step at a time.",
        labels=("Yield one step", "Cut the locking brace", "Ram the shield"),
        flavors=("You preserve room to breathe.", "You attack the hinge buried in stone.", "You meet the golden wall shoulder-first."),
        failure_status="bleed",
    ),
    "aegis_last_stand": _variety_mechanic(
        mechanic_id="aegis_last_stand",
        archetype="second_life",
        trigger_round=6,
        title="Every fallen shield rises around the Warden",
        description="Old defenses answer one final command from the reliquary.",
        labels=("Wait for the formation", "Topple the nearest shield", "Break through the center"),
        flavors=("You study where the shields overlap.", "You turn one guardian into a gap.", "You challenge the whole formation at once."),
        failure_status="silence",
    ),
    "heartspire_blood_tithe": _variety_mechanic(
        mechanic_id="heartspire_blood_tithe",
        archetype="gamble",
        trigger_round=3,
        title="The Heartspire demands a blood tithe",
        description="A crimson scale hangs between your pulse and the machine's.",
        labels=("Offer a shallow cut", "Tip the scale with iron", "Refuse the tithe"),
        flavors=("You give the mechanism almost nothing.", "You lay your pick across the crimson pan.", "You strike the scale instead of paying."),
        failure_status="bleed",
    ),
    "heartspire_crimson_pulse": _variety_mechanic(
        mechanic_id="heartspire_crimson_pulse",
        archetype="channel_aoe",
        trigger_round=4,
        title="The Heartspire releases a crimson pulse",
        description="The chamber contracts around the beat of its suspended core.",
        labels=("Breathe between beats", "Pierce the outer vessel", "Strike on the pulse"),
        flavors=("You move only in the still interval.", "You vent pressure from a side vein.", "You drive your pick in as the chamber contracts."),
        failure_status="bleed",
    ),
    "emberwright_molten_anvil": _variety_mechanic(
        mechanic_id="emberwright_molten_anvil",
        archetype="charge_telegraph",
        trigger_round=3,
        title="The Emberwright tips a molten anvil upright",
        description="The glowing slab begins to fall across the whole work lane.",
        labels=("Back beyond its reach", "Cool the lower edge", "Slide beneath it"),
        flavors=("You retreat past the anvil's shadow.", "You throw loose slag against the hottest edge.", "You dive under the descending iron."),
        failure_status="burn",
    ),
    "emberwright_scrap_volley": _variety_mechanic(
        mechanic_id="emberwright_scrap_volley",
        archetype="summon_swarm",
        trigger_round=4,
        title="The Emberwright feeds scrap into the forge vents",
        description="A storm of rivets and red-hot teeth erupts toward you.",
        labels=("Shelter behind the anvil", "Jam the nearest vent", "Bat the scrap aside"),
        flavors=("You crouch behind cooling iron.", "You wedge your pick into the vent mouth.", "You meet the metal storm swing for swing."),
        failure_status="burn",
    ),

    # ================================================================
    # VARIETY EXPANSION — PINNACLE PHASES
    # ================================================================
    "king_crownfall": _variety_mechanic(
        mechanic_id="king_crownfall",
        archetype="channel_big_hit",
        trigger_round=4,
        title="The Forgotten King's crown falls like a blade",
        description="Ancient gold widens into a descending ring of judgment.",
        labels=("Kneel beneath the arc", "Strike the cracked jewel", "Catch the crown"),
        flavors=("You lower yourself without bowing.", "You aim for the crown's dead center.", "You raise your pick beneath royal gold."),
        failure_status="bleed",
    ),
    "king_royal_hunt": _variety_mechanic(
        mechanic_id="king_royal_hunt",
        archetype="charge_telegraph",
        trigger_round=4,
        title="The Crowned Hunger declares a royal hunt",
        description="Ghostly hounds pace out a circle while the King lowers his spear.",
        labels=("Stay outside the hounds", "Turn the lead hound", "Charge the hunter"),
        flavors=("You keep the spectral pack in sight.", "You cut across the first hound's path.", "You make the prey run forward."),
        failure_status="bleed",
    ),
    "king_final_judgment": _variety_mechanic(
        mechanic_id="king_final_judgment",
        archetype="mark_delayed",
        trigger_round=5,
        title="The Last Breath of Kings names your sentence",
        description="The words carve themselves into the stone beneath you.",
        labels=("Step outside the inscription", "Erase the final word", "Deliver your own verdict"),
        flavors=("You leave the sentence unfinished.", "You grind the last rune into dust.", "You answer judgment with a raised pick."),
        failure_status="silence",
    ),
    "hollow_empty_gaze": _variety_mechanic(
        mechanic_id="hollow_empty_gaze",
        archetype="reality_warp",
        trigger_round=4,
        title="Hollowforged opens an eye made of tunnel",
        description="Looking into it shows the chamber continuing through your body.",
        labels=("Look at its shadow", "Blind the stone eye", "Stare through it"),
        flavors=("You follow what the impossible eye cannot see.", "You fill the hollow lens with shattered rock.", "You force your gaze down the endless shaft."),
        failure_status="silence",
    ),
    "hollow_stolen_face": _variety_mechanic(
        mechanic_id="hollow_stolen_face",
        archetype="reform_predict",
        trigger_round=4,
        title="Hollowforged Reformed wears your face",
        description="The mineral copy smiles just before matching your stance.",
        labels=("Change your footing", "Break the copied jaw", "Mirror the mirror"),
        flavors=("You move unlike yourself.", "You strike the familiar face at its seam.", "You copy the copy until one of you breaks."),
        failure_status="bleed",
    ),
    "hollow_silence_between": _variety_mechanic(
        mechanic_id="hollow_silence_between",
        archetype="bind_debuff",
        trigger_round=5,
        title="Hollowforged Pluralized removes the sound between voices",
        description="The chorus becomes pressure, crushing every unspoken thought.",
        labels=("Hold one clear thought", "Strike between two voices", "Add your own voice"),
        flavors=("You keep one small idea entirely yours.", "You attack the instant the chorus changes speaker.", "You shout until the chamber must include you."),
        failure_status="silence",
    ),
    "digger_mirror_swing": _variety_mechanic(
        mechanic_id="digger_mirror_swing",
        archetype="weapon_duel",
        trigger_round=4,
        title="The First Digger copies your oldest swing",
        description="He learned it before you were born and knows where it ends.",
        labels=("Abandon the swing", "Change hands mid-arc", "Finish faster"),
        flavors=("You let the remembered motion die.", "You turn the familiar blow inside out.", "You race him to the same ending."),
        failure_status="bleed",
    ),
    "digger_faultline_step": _variety_mechanic(
        mechanic_id="digger_faultline_step",
        archetype="phase_chase",
        trigger_round=4,
        title="The Digger Unbound steps through a fault line",
        description="His hands emerge from three cracks before the rest of him chooses one.",
        labels=("Guard the widest crack", "Collapse the center fault", "Follow his hand through"),
        flavors=("You wait where a body could actually fit.", "You stamp the unstable seam shut.", "You grab the reaching wrist and dive."),
        failure_status="burn",
    ),
    "digger_last_excavation": _variety_mechanic(
        mechanic_id="digger_last_excavation",
        archetype="environmental_collapse",
        trigger_round=5,
        title="The Digger Eternal begins the last excavation",
        description="Every wall peels backward toward a shaft older than the mine.",
        labels=("Brace the nearest wall", "Break his digging rhythm", "Dig toward him"),
        flavors=("You hold one piece of the chamber in place.", "You strike between the eternal blows.", "You race the first digger into the dark."),
        failure_status="bleed",
    ),
}


# ---------------------------------------------------------------------------
# Lookups
# ---------------------------------------------------------------------------

def get_mechanic(mechanic_id: str) -> BossMechanic | None:
    return MECHANIC_REGISTRY.get(mechanic_id)
