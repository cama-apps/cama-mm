"""Boss definitions, combat math, phase/pinnacle revamp data, and dialogue.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

from dataclasses import dataclass

from services.dig_data.artifacts import RELICS

# ---------------------------------------------------------------------------
# Boss Definitions
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class BossDef:
    """Immutable definition for a layer boss."""
    depth: int
    name: str
    title: str
    ascii_art: str
    dialogue: list[str]             # 5 stages: threatening -> absurd
    boss_id: str = ""               # stable unique identifier (e.g. "grothak", "pudge")
    mechanic_pool: tuple[str, ...] = ()  # keys into MECHANIC_REGISTRY; one rolled per fight
    stinger_id: str = ""            # key into STINGER_REGISTRY; fires on player loss
    prestige_required: int = 0      # min prestige level for this boss to appear in the pool
    # Per-boss outcome flavor pools. Empty defaults fall back to the generic
    # GENERIC_VICTORY_LINES / GENERIC_DEFEAT_LINES pools at render time. Use
    # the {boss} token to reference the boss name.
    victory_lines: tuple[str, ...] = ()
    defeat_lines: tuple[str, ...] = ()


# Fallback flavor pools — used when a BossDef doesn't define its own.
# Atmospheric, no mechanic exposition (per the dig flavor preference).
GENERIC_VICTORY_LINES: tuple[str, ...] = (
    "{boss} folds. The dark settles back into stone.",
    "{boss} sinks to the floor without a word.",
    "Something in {boss} unwinds, then stills.",
    "You step over {boss}'s outline. The hollow opens.",
)
GENERIC_DEFEAT_LINES: tuple[str, ...] = (
    "{boss} watches you stagger back the way you came.",
    "The blow lands. {boss} doesn't follow — doesn't need to.",
    "You don't remember falling. You remember the silence after.",
    "{boss} resumes its waiting. The tunnel resumes its quiet.",
)


BOSSES: dict[int, BossDef] = {
    25: BossDef(
        depth=25,
        name="Grothak the Unbreakable",
        title="Guardian of the Shallows",
        ascii_art=(
            "########.....########\n"
            "#.......|   |.......#\n"
            "#.......,-^-,.......#\n"
            "#....../ o o \\.......#\n"
            "#.....| (___) |.....#\n"
            "#......\\ === /.......#\n"
            "#.......'---'.......#\n"
            "#.........@.........#\n"
            "#####################"
        ),
        dialogue=[
            "You dare enter MY dirt?! I'll crush you like the worm you are!",
            "Again?! My back already hurts from the last fight... ugh.",
            "Look, can we reschedule? My chiropractor says I need rest.",
            "YOU AGAIN?! I literally just sat down!",
            "Fine. Hit me. I can't feel anything below the waist anyway.",
        ],
        boss_id="grothak",
        mechanic_pool=("grothak_earthquake", "grothak_crumble_wall"),
        stinger_id="grothak_crumble",
        victory_lines=(
            "{boss} sinks down with a long, tired sigh. Finally a nap.",
            "{boss} cracks once and goes quiet. The dirt accepts him back.",
            "You climb past {boss}'s shoulder. He doesn't get up.",
            "{boss} mutters something about his back and lies down.",
        ),
        defeat_lines=(
            "{boss} flicks you back like a beetle. The dust takes a while to settle.",
            "You wake up uphill of where you fell. {boss} is already snoring.",
            "{boss} barely moved. You moved a lot.",
            "Something in your ribs argues with something in your knees. {boss} grunts.",
        ),
    ),
    50: BossDef(
        depth=50,
        name="Crystalia the Refracted",
        title="Mistress of Perfect Angles",
        ascii_art=(
            "   /\\_/\\\n"
            "  ( o.o )\n"
            " />diamonds<\\\n"
            " \\_______/\n"
            "   |||||"
        ),
        dialogue=[
            "Your asymmetrical face offends me. Prepare to be geometrically corrected!",
            "You're back and you STILL haven't fixed that crooked nose?!",
            "Please just stand three degrees to the left... no, MY left. UGH.",
            "YOU AGAIN?! Do you know how long it took to re-align these crystals?!",
            "I give up. Nothing is symmetrical anymore. Not even my will to fight.",
        ],
        boss_id="crystalia",
        mechanic_pool=("crystalia_prism", "crystalia_shatter"),
        stinger_id="crystalia_shard",
        victory_lines=(
            "{boss} shatters along a perfect axis. She would have appreciated the angle.",
            "Light fractures off {boss}'s last facet and goes elsewhere.",
            "{boss} chimes once, beautifully, and stops.",
            "The crystal lattice unravels. Symmetry was never the point.",
        ),
        defeat_lines=(
            "{boss} catches your reflection wrong and the room turns inside out.",
            "Your own face stares back from a thousand shards. None of them help.",
            "{boss} corrects you, geometrically. It hurts in two dimensions at once.",
            "Light enters {boss} and does not leave. Neither do you, for a while.",
        ),
    ),
    75: BossDef(
        depth=75,
        name="Magmus Rex",
        title="Sovereign of the Molten Depths",
        ascii_art=(
            "  ~*~*~\n"
            " {(O  O)}\n"
            " {  <>  }\n"
            " {\\_^^_/}\n"
            "  ~~~~~"
        ),
        dialogue=[
            "BURN, MORTAL! I am the flame that— actually, can we do this later?",
            "Ugh, not you again. Do you know how hard it is to get PTO down here?",
            "I put in for vacation THREE CENTURIES AGO. HR hasn't responded.",
            "YOU AGAIN?! I was literally packing my bags for Bali!",
            "I'm just gonna lie here. Lava is basically a hot tub, right? ...right?",
        ],
        boss_id="magmus_rex",
        mechanic_pool=("magmus_eruption", "magmus_meteor"),
        stinger_id="magmus_burn",
        victory_lines=(
            "{boss} cools to a slow red and stops complaining.",
            "The lava settles. {boss} mutters about Bali and dims.",
            "{boss} sinks back into the floor he came out of.",
            "A last belch of smoke. {boss} is officially on PTO.",
        ),
        defeat_lines=(
            "{boss} doesn't even sit up. He just sets things on fire.",
            "The heat finds parts of you that aren't supposed to feel heat.",
            "{boss} yawns and the chamber turns orange.",
            "You retreat smoking. {boss} goes back to sulking.",
        ),
    ),
    100: BossDef(
        depth=100,
        name="The Void Warden",
        title="Keeper of the Final Dark",
        ascii_art=(
            "  .o0O0o.\n"
            " (  ???  )\n"
            " |  _V_  |\n"
            " ( '---' )\n"
            "  `o0O0o'"
        ),
        dialogue=[
            "You gaze into the abyss, and the abyss... wonders why it bothers.",
            "Oh. You again. Do I even exist if no one digs here?",
            "I've been guarding nothing for eons. What's the point, really?",
            "YOU AGAIN?! Is this all there is? Darkness and... diggers?",
            "You know what? Take the void. I'm going to go find myself.",
        ],
        boss_id="void_warden",
        mechanic_pool=("voidwarden_collapse", "voidwarden_silence"),
        stinger_id="void_collapse",
        victory_lines=(
            "{boss} blinks out. The dark stays. The point is debatable.",
            "{boss} dissolves into the kind of quiet that has weight.",
            "{boss} looks at you, then looks at nothing, then is nothing.",
            "{boss} steps backwards into a place that isn't a place.",
        ),
        defeat_lines=(
            "{boss} doesn't strike. It just stops being looked at, and you fall.",
            "The void around {boss} folds you somewhere else, briefly, badly.",
            "Nothing happens for a long time. Then you're a long way back up.",
            "{boss} blinks. You wake up below where you started.",
        ),
    ),
    150: BossDef(
        depth=150,
        name="Sporeling Sovereign",
        title="The One Who Grows",
        ascii_art=(
            "  .oO@Oo.\n"
            " /  we   \\\n"
            "( are one )\n"
            " \\  all  /\n"
            "  'oO@Oo'"
        ),
        dialogue=[
            "We are the soil and the soil is us. You trespass on ourselves.",
            "You return. We have grown since last you came. We remember your footsteps.",
            "We considered offering you tea. Then we remembered we are mushrooms.",
            "YOU AGAIN. We were in the middle of photosynthesis. ...Wait. We don't do that.",
            "Fine. We yield. Would you like a mushroom recipe? We have thousands.",
        ],
        boss_id="sporeling_sovereign",
        mechanic_pool=("sporeling_cloud", "sporeling_roots"),
        stinger_id="sporeling_rot",
        victory_lines=(
            "{boss} releases a final breath of pollen, then settles into mulch.",
            "The colony quiets. {boss} is now the soil it always claimed to be.",
            "{boss} loosens at the edges and rejoins the floor.",
            "Something old roots itself, gently, and stops moving.",
        ),
        defeat_lines=(
            "{boss}'s spores find your lungs first. Everything else is paperwork.",
            "Roots tighten around your boots. {boss} hums, satisfied.",
            "The chamber smells like wet stone and old growth. You wake up moss-warm.",
            "{boss} barely stirred. The colony did the work.",
        ),
    ),
    200: BossDef(
        depth=200,
        name="Chronofrost",
        title="The Still Moment",
        ascii_art=(
            "  *  . *  .\n"
            " / frozen  \\\n"
            "| t i m e  |\n"
            " \\ stands /\n"
            "  *  . *  ."
        ),
        dialogue=[
            "You arrive exactly when I expected. I've been waiting since before you were born.",
            "We've done this before. You just don't remember yet. I envy that.",
            "I could tell you how this ends but you wouldn't believe me. I barely do.",
            "YOU AGAIN. Or is it still? Time is a suggestion down here.",
            "Go. I've seen every possible outcome and in most of them you win anyway.",
        ],
        boss_id="chronofrost",
        mechanic_pool=("chronofrost_still", "chronofrost_rewind"),
        stinger_id="chronofrost_stillness",
        victory_lines=(
            "{boss} smiles the smile of someone who saw this coming.",
            "{boss} stops, exactly when expected. Time exhales.",
            "{boss} folds out of the moment. The chamber loses a tense.",
            "{boss} looks at you for an instant that lasts a year, then is gone.",
        ),
        defeat_lines=(
            "{boss} pauses. Time pauses. You don't.",
            "The minute repeats. So does your mistake.",
            "{boss} watches the same blow land four times before letting it.",
            "You look up. Someone is wearing your face. {boss} is patient.",
        ),
    ),
    275: BossDef(
        depth=275,
        name="The Nameless Depth",
        title="[REDACTED]",
        ascii_art=(
            "  . . . . .\n"
            " .         .\n"
            " .  ?   ?  .\n"
            " .    _    .\n"
            "  . . . . ."
        ),
        dialogue=[
            "I was you, once. Before the digging consumed me.",
            "Your tunnel. I know its name. I know all the names.",
            "You dig to find something. I dug to forget something. We are the same.",
            "YOU AGAIN. Or am I you again? The distinction stopped mattering at depth 250.",
            "Take the hollow. It was always yours. I was just keeping it warm.",
        ],
        boss_id="nameless_depth",
        mechanic_pool=("nameless_whisper", "nameless_silence"),
        stinger_id="nameless_erase",
        victory_lines=(
            "{boss} steps aside. The hollow that's left has the shape of a person.",
            "{boss} dissolves without complaint. You don't ask whose name was on it.",
            "Something underfoot lets go. {boss} is no longer a thing you remember.",
            "{boss} smiles. You think it might be at you. You hope it isn't.",
        ),
        defeat_lines=(
            "{boss} doesn't fight. {boss} just keeps knowing your name.",
            "You forget why you came. {boss} reminds you, gently, the wrong way.",
            "Something unwrites itself. You wake up further up than you should be.",
            "{boss} watches you go. You feel watched for hours after.",
        ),
    ),
}


# ---------------------------------------------------------------------------
# New Dota-themed bosses (2 per tier, sharing each tier with 1 grandfathered
# boss). A tunnel rolls one boss per tier when it first crosses the milestone
# and locks that pick for the run (see boss_progress JSON shape on the
# tunnels table).
# ---------------------------------------------------------------------------

_DOTA_BOSSES: dict[str, BossDef] = {
    "pudge": BossDef(
        depth=25,
        boss_id="pudge",
        name="The Butcher",
        title="Stitched-Together Hooker",
        ascii_art=(
            "   _____\n"
            "  /     \\\n"
            " | X o X |~~===>\n"
            "  \\_____/   (hook)\n"
            "   /|||\\\n"
        ),
        dialogue=[
            "FRESH MEAT!",
            "Oh, you came back. The last one tasted like regret.",
            "I skipped lunch for this. You'd better be worth it.",
            "YOU AGAIN?! My hook is blunt from you alone.",
            "Fine. Walk past. I'm too tired to even taunt.",
        ],
        mechanic_pool=("pudge_hook", "pudge_rot"),
        stinger_id="pudge_drag",
    ),
    "ogre_magi": BossDef(
        depth=25,
        boss_id="ogre_magi",
        name="The Twin-Skulled",
        title="Two Heads, Zero Plans",
        ascii_art=(
            "   (o)(o)\n"
            "  /      \\\n"
            " |  urrrk |\n"
            "  \\      /\n"
            "   \\____/\n"
            "    ||||\n"
        ),
        dialogue=[
            "One of us casts! The other forgets!",
            "We saw you yesterday. We forgot you today. Hi again!",
            "Left head wants to fight. Right head wants nachos.",
            "YOU AGAIN! ...who? Oh right. YOU.",
            "Both heads tired. Both heads say: yield.",
        ],
        mechanic_pool=("ogre_multicast", "ogre_fireblast"),
        stinger_id="ogre_blast",
    ),
    "crystal_maiden": BossDef(
        depth=50,
        boss_id="crystal_maiden",
        name="The Frostbinder",
        title="Cold in the Best Way",
        ascii_art=(
            "    ,-'-.\n"
            "   ( *.* )\n"
            "  /~|\"|~\\\n"
            " / brrrr \\\n"
            "   | | |\n"
        ),
        dialogue=[
            "Stay a while. You'll be cold forever.",
            "You came back? My mana hasn't even regenerated.",
            "Okay, listen. I just did my hair. Please die quickly.",
            "YOU AGAIN?! I was literally mid-ult.",
            "Okay fine, I'll come quietly. But stop ganking me.",
        ],
        mechanic_pool=("cm_frostbite", "cm_freezing_field"),
        stinger_id="cm_freeze",
    ),
    "tusk": BossDef(
        depth=50,
        boss_id="tusk",
        name="The Ice Warlord",
        title="The Walrus With A Plan",
        ascii_art=(
            "   .---.\n"
            "  ( o o )\n"
            " / |===| \\\n"
            " \\_______/\n"
            "  ~~~snow~~~\n"
        ),
        dialogue=[
            "You ever been yeeted by a walrus? You're about to.",
            "Snowball's out. Good luck.",
            "I will kick you so hard you forget your own depth.",
            "YOU AGAIN?! I'm out of snow. Give me a minute.",
            "Fine. Go. Tell your friends a walrus sent you.",
        ],
        mechanic_pool=("tusk_snowball", "tusk_walrus_punch"),
        stinger_id="tusk_kick",
    ),
    "lina": BossDef(
        depth=75,
        boss_id="lina",
        name="The Scorchwitch",
        title="She Who Brings the Heat",
        ascii_art=(
            "   ~*~*~\n"
            "   (` )\n"
            "   /\\_/\\\n"
            "  ( >_< )\n"
            "   /   \\\n"
        ),
        dialogue=[
            "My fingers are warming up. Say goodbye.",
            "Oh look, you. Again. I'll try to kill you differently this time.",
            "I'm low-key tired. Let's just one-shot this.",
            "YOU AGAIN?! My mana bar has trust issues.",
            "Ugh. Fine. Take the depth. My hair's frizzed anyway.",
        ],
        mechanic_pool=("lina_laguna", "lina_dragon_slave"),
        stinger_id="lina_scorch",
    ),
    "doom": BossDef(
        depth=75,
        boss_id="doom",
        name="The Deathbringer",
        title="Lord of the Avernus",
        ascii_art=(
            "   .---.\n"
            "  /X X X\\\n"
            " |  ___  |\n"
            "  \\_____/\n"
            "   ||||| \n"
        ),
        dialogue=[
            "Silence. The end approaches.",
            "You return. I am unimpressed.",
            "Every one of your digs extends my work week.",
            "YOU AGAIN. Even the damned get tired.",
            "Go. I need a holiday from you.",
        ],
        mechanic_pool=("doom_mark", "doom_scorched_earth"),
        stinger_id="doom_brand",
    ),
    "spectre": BossDef(
        depth=100,
        boss_id="spectre",
        name="The Dread Shade",
        title="The Dagger in the Dark",
        ascii_art=(
            "   _____\n"
            "  /     \\\n"
            " |  o o  |\n"
            "  \\  V  /\n"
            "   \\___/\n"
            "    ~||~\n"
        ),
        dialogue=[
            "I have already struck you. You just haven't noticed.",
            "You keep returning. I never actually leave.",
            "We are the same wound.",
            "YOU AGAIN. I am, as ever.",
            "Go. My work is never done anyway.",
        ],
        mechanic_pool=("spectre_haunt", "spectre_dagger"),
        stinger_id="spectre_haunting",
    ),
    "void_spirit": BossDef(
        depth=100,
        boss_id="void_spirit",
        name="The Astral Echo",
        title="Dimensional Tourist",
        ascii_art=(
            "    .\" \".\n"
            "   ( *.* )\n"
            "    \\=|=/\n"
            "  ~~/   \\~~\n"
            "    /   \\\n"
        ),
        dialogue=[
            "I stepped sideways through space to kill you. Worth it.",
            "Back from a different dimension. You still here?",
            "I know seven of your tunnels. Yours is the worst one.",
            "YOU AGAIN?! I am literally everywhere else.",
            "Fine. I'll take this dimension off.",
        ],
        mechanic_pool=("void_spirit_step", "void_spirit_aether"),
        stinger_id="void_spirit_exile",
    ),
    "treant_protector": BossDef(
        depth=150,
        boss_id="treant_protector",
        name="The Elder Grove",
        title="Old Growth, Old Grudges",
        ascii_art=(
            "      /\\\n"
            "     /  \\\n"
            "    /\\/\\ \\\n"
            "    /   \\ \\\n"
            "   / (0) \\\n"
            "    | | |\n"
        ),
        dialogue=[
            "You dig. I grow. One of us is patient.",
            "Again. Trees have long memories.",
            "Every time you return I have more rings.",
            "YOU AGAIN. I am older than your tunnel.",
            "Go. Even trees can grow tired.",
        ],
        mechanic_pool=("treant_overgrowth", "treant_leech_seed"),
        stinger_id="treant_entangle",
    ),
    "broodmother": BossDef(
        depth=150,
        boss_id="broodmother",
        name="The Nestmother",
        title="Nine Hundred Hungry Children",
        ascii_art=(
            "    /\\ /\\\n"
            "   (oOOo)\n"
            "  / '--' \\\n"
            " ~~~webs~~~\n"
            "    \\\\||//\n"
        ),
        dialogue=[
            "My children are hungry. Please don't run.",
            "You keep bringing yourself back. Thoughtful of you.",
            "I still haven't named half of them. Want to help?",
            "YOU AGAIN?! I was in the middle of spinning.",
            "Fine. Go. Leave us to our weaving.",
        ],
        mechanic_pool=("broodmother_spawn", "broodmother_web"),
        stinger_id="broodmother_webbing",
    ),
    "faceless_void": BossDef(
        depth=200,
        boss_id="faceless_void",
        name="The Timeless One",
        title="There Is No Timing Like His Timing",
        ascii_art=(
            "   _______\n"
            "  /       \\\n"
            " |   _ _   |\n"
            "  \\  /-\\  /\n"
            "   \\_____/\n"
            "    time\n"
        ),
        dialogue=[
            "I saw this coming. Literally.",
            "You again. I was expecting you five seconds ago.",
            "I'll freeze the moment and walk away. Have fun.",
            "YOU AGAIN. My cooldown is up, regrettably.",
            "Go. You were going to win this one anyway.",
        ],
        mechanic_pool=("faceless_void_chrono", "faceless_void_backtrack"),
        stinger_id="void_chrono",
    ),
    "weaver": BossDef(
        depth=200,
        boss_id="weaver",
        name="The Skitterwing",
        title="The One Who Unpicks",
        ascii_art=(
            "   .-.\n"
            "  ( ^ )\n"
            "  /|X|\\\n"
            " / | | \\\n"
            "  ~===~\n"
        ),
        dialogue=[
            "I will pull one thread. You will unravel.",
            "Oh. You. Again. I hadn't even finished stitching.",
            "Time-lapse away now and we both save energy.",
            "YOU AGAIN?! I reset my own timeline to rest.",
            "Take the depth. I'll weave it back later.",
        ],
        mechanic_pool=("weaver_timelapse", "weaver_shukuchi"),
        stinger_id="weaver_unmake",
    ),
    "oracle": BossDef(
        depth=275,
        boss_id="oracle",
        name="The Blindfolded Seer",
        title="Seer of Bad Bets",
        ascii_art=(
            "   .-\"\"\"-.\n"
            "  / ? ? ? \\\n"
            " | o . o |\n"
            "  \\_=_=_/\n"
            "    |||\n"
        ),
        dialogue=[
            "I have already decided which one of us wins.",
            "You? Again? I foresaw it. And still find it tedious.",
            "Let's flip for it. Pick a side. Both sides lose.",
            "YOU AGAIN. I predicted this too.",
            "Go. The coin is tired.",
        ],
        mechanic_pool=("oracle_fortune", "oracle_false_promise"),
        stinger_id="oracle_fate",
    ),
    "terrorblade": BossDef(
        depth=275,
        boss_id="terrorblade",
        name="The Sundered Prince",
        title="Betrayer and Sunderer",
        ascii_art=(
            "     /\\_/\\\n"
            "    ( >_< )\n"
            "   _/|'-'|\\_\n"
            "  |__|---|__|\n"
            "     /|v|\\\n"
        ),
        dialogue=[
            "I will trade lives with you. You will not like yours.",
            "You returned. Willingly. I admire the theater.",
            "One more trade. Then we talk severance.",
            "YOU AGAIN?! My mirror image is tired.",
            "Take the hollow. Cleave yourself out of it.",
        ],
        mechanic_pool=("terrorblade_sunder", "terrorblade_metamorphosis"),
        stinger_id="terrorblade_sundering",
    ),
    # ----- Late-prestige additions (only appear at prestige>=3) -----
    "xalatath": BossDef(
        depth=150,
        boss_id="xalatath",
        name="Xal'atath",
        title="Voidweaver",
        ascii_art=(
            "   . - . - .\n"
            "  -  ((  )) -\n"
            " .   \\___/   .\n"
            "  -  ~vvv~  -\n"
            "   . - . - .\n"
        ),
        dialogue=[
            "You carry something. I can taste the shape of it.",
            "Again. The carrying hasn't helped.",
            "Still holding it? Set it down. It weighs differently than you think.",
            "STILL. After everything I said.",
            "Set it down. I am bored of asking.",
        ],
        mechanic_pool=("xalatath_void_pull", "xalatath_whisper_madness"),
        stinger_id="xalatath_unraveling",
        prestige_required=3,
        victory_lines=(
            "{boss} folds. The whisper folds with her. Stone stops carrying it.",
            "{boss} unravels into syllables that don't fit anywhere. They leave.",
            "{boss} lays down quietly. You realize you can hear yourself again.",
            "Something in {boss}'s mouth stops moving. The cave gets less wrong.",
        ),
        defeat_lines=(
            "{boss} keeps speaking. The words go in. They don't come out.",
            "You leave. You leave it. You don't know what 'it' was. {boss} is calmer now.",
            "Something walks home in your boots. {boss} is patient.",
            "{boss} folds the whisper back into her teeth. She'll save it for later.",
        ),
    ),
    "lilith": BossDef(
        depth=200,
        boss_id="lilith",
        name="Lilith",
        title="Daughter of Hatred",
        ascii_art=(
            "  \\  ___  /\n"
            " \\ /     \\ /\n"
            "  | (* *) |\n"
            "  | \\___/ |\n"
            "   \\\\|||//\n"
            "    \\ v /\n"
        ),
        dialogue=[
            "All things return to hatred. You are no exception.",
            "You return. Hatred is patient. Hatred waited.",
            "Bleed. The deep drinks bleed. The deep is grateful.",
            "YOU. AGAIN. My hatred is tireless. Is yours?",
            "Come, then. Cycle is cycle. Eternity is exhausting.",
        ],
        mechanic_pool=("lilith_blood_nova", "lilith_wing_descent"),
        stinger_id="lilith_hemorrhage",
        prestige_required=3,
        victory_lines=(
            "{boss} kneels. Hatred has a posture and it doesn't fit her any more.",
            "{boss} bleeds out gracefully — no more graceful than blood allows.",
            "{boss} folds her wings around herself and quiets.",
            "{boss} loses interest. The hatred stays. Hers leaves.",
        ),
        defeat_lines=(
            "{boss} leans down to look at you. You become a thought she has.",
            "Hatred holds the door open for you on the way out. {boss} watches you take it.",
            "{boss} lets you live. That itself is a kind of ruin.",
            "You look up; {boss} is gone. The room is wet. You walk back.",
        ),
    ),
    "underlord": BossDef(
        depth=275,
        boss_id="underlord",
        name="The Pit Lord",
        title="Lord of the Underworld",
        ascii_art=(
            "    ______\n"
            "   /      \\\n"
            "  | >_  _< |\n"
            "  |   __   |\n"
            "   \\______/\n"
            "    ||||||\n"
            "   /  pit \\\n"
        ),
        dialogue=[
            "You're not the first. You won't be the last. Fight.",
            "Back. Good. Weakness is inefficiency. Fix it.",
            "I don't negotiate. I have a pit for that.",
            "AGAIN. I respect the persistence. Not much else.",
            "Just hit me. The pit gets bored.",
        ],
        mechanic_pool=("underlord_pit_pull", "underlord_firestorm"),
        stinger_id="underlord_atrophy",
        prestige_required=3,
        victory_lines=(
            "{boss} sets down his weight and grunts. The grunt is a kind of respect.",
            "{boss} steps back from his pit. He'll re-dig it tomorrow.",
            "{boss} folds his arms and looks past you, already bored.",
            "{boss} grunts: 'Hm. Earned it.' He doesn't stand back up.",
        ),
        defeat_lines=(
            "{boss} hauls you up by the collar and sets you on the path back. It is a long path.",
            "The pit pulled. {boss} watched. He didn't even strike. He didn't have to.",
            "{boss} shakes his head once and turns away. The pit walks you home.",
            "{boss} says nothing. The pit closes. You're on the wrong side of it.",
        ),
    ),
}


# BOSSES_BY_TIER: new canonical per-tier grouping. The first entry per tier
# is the grandfathered fantasy boss (preserved from ``BOSSES``); the remaining
# entries are the Dota-themed additions. All gameplay code that selects
# which boss a tunnel faces should go through this table via
# ``get_boss_pool_for_tier`` or ``get_boss_by_id``.
BOSSES_BY_TIER: dict[int, list[BossDef]] = {
    25:  [BOSSES[25],  _DOTA_BOSSES["pudge"],            _DOTA_BOSSES["ogre_magi"]],
    50:  [BOSSES[50],  _DOTA_BOSSES["crystal_maiden"],   _DOTA_BOSSES["tusk"]],
    75:  [BOSSES[75],  _DOTA_BOSSES["lina"],             _DOTA_BOSSES["doom"]],
    100: [BOSSES[100], _DOTA_BOSSES["spectre"],          _DOTA_BOSSES["void_spirit"]],
    150: [BOSSES[150], _DOTA_BOSSES["treant_protector"], _DOTA_BOSSES["broodmother"],  _DOTA_BOSSES["xalatath"]],
    200: [BOSSES[200], _DOTA_BOSSES["faceless_void"],    _DOTA_BOSSES["weaver"],       _DOTA_BOSSES["lilith"]],
    275: [BOSSES[275], _DOTA_BOSSES["oracle"],           _DOTA_BOSSES["terrorblade"],  _DOTA_BOSSES["underlord"]],
}


# BOSSES_BY_ID: flat lookup, boss_id -> BossDef.
BOSSES_BY_ID: dict[str, BossDef] = {
    boss.boss_id: boss
    for tier_list in BOSSES_BY_TIER.values()
    for boss in tier_list
}


def get_boss_pool_for_tier(tier: int, prestige_level: int = 99) -> list[BossDef]:
    """Return the list of candidate BossDefs for the given tier depth.

    Bosses with ``prestige_required`` above ``prestige_level`` are filtered
    out. The default of 99 is fail-open: an unforeseen caller that omits
    the argument will see the full pool rather than silently hide gated
    bosses from a player who should see them. The trade-off is that any
    caller that surfaces boss names to a player MUST pass that player's
    real prestige, or names of gated bosses can leak.
    """
    pool = BOSSES_BY_TIER.get(tier, [])
    return [b for b in pool if b.prestige_required <= prestige_level]


def get_boss_by_id(boss_id: str) -> BossDef | None:
    """Return the BossDef with the given boss_id, or None."""
    return BOSSES_BY_ID.get(boss_id)


# Boss fight mechanics ────────────────────────────────────────────
# Bosses are resolved as a multi-round HP duel: player and boss alternate
# turns (player first), each rolling their tier's hit chance and dealing
# damage on a hit. Whoever reaches 0 HP first loses. Player loss = forfeit
# wager + cave-in to the previous milestone.
#
# Per-tier stats (player_hp, boss_hp, player_hit, player_dmg, boss_hit, boss_dmg).
# Reckless is tuned for WAGERED play. Free-fight reckless clamps to the
# PLAYER_HIT_FLOOR (0.10 * BOSS_FREE_FIGHT_ACCURACY_MOD = 0.06, floored to
# 0.05) which is intentionally near-impossible: high-stakes wager only.
BOSS_DUEL_STATS: dict[str, dict[str, float]] = {
    "cautious": {"player_hp": 5, "boss_hp": 4, "player_hit": 0.60, "player_dmg": 1, "boss_hit": 0.30, "boss_dmg": 1, "crit_chance": 0.00, "crit_bonus": 0},
    "bold":     {"player_hp": 3, "boss_hp": 5, "player_hit": 0.42, "player_dmg": 2, "boss_hit": 0.45, "boss_dmg": 1, "crit_chance": 0.15, "crit_bonus": 1},
    "reckless": {"player_hp": 2, "boss_hp": 6, "player_hit": 0.18, "player_dmg": 3, "boss_hit": 0.60, "boss_dmg": 1, "crit_chance": 0.30, "crit_bonus": 1},
}

# Boss difficulty curve — hand-tuned lookup tables. Replaces the prior
# linear-formula scaling. The tables are the single source of truth: each
# cell is added to the boss base+archetype stats, and a Monte-Carlo
# simulation confirmed the resulting curve. Tune by editing entries.
BOSS_TIER_BONUS: dict[int, dict[str, float]] = {
    # boundary depth: {boss_hp_add, boss_hit_add, boss_dmg_add, player_hit_pen}
    # Mid-late rows (150/200/275) are nudged a touch tougher — those tiers
    # had become a bit too smooth once gear caught up.
    25:  {"hp": 0,  "hit": 0.00, "dmg": 0, "pen": 0.00},
    50:  {"hp": 1,  "hit": 0.00, "dmg": 0, "pen": 0.01},
    75:  {"hp": 2,  "hit": 0.01, "dmg": 0, "pen": 0.02},
    100: {"hp": 3,  "hit": 0.03, "dmg": 0, "pen": 0.04},
    150: {"hp": 6,  "hit": 0.05, "dmg": 0, "pen": 0.06},
    200: {"hp": 6,  "hit": 0.06, "dmg": 0, "pen": 0.07},
    275: {"hp": 7,  "hit": 0.07, "dmg": 0, "pen": 0.07},
    350: {"hp": 9,  "hit": 0.10, "dmg": 0, "pen": 0.06},   # pinnacle: HP grind, no dmg cliff
}
BOSS_PRESTIGE_BONUS: dict[int, dict[str, float]] = {
    # prestige: {boss_hp_add, boss_hit_add, boss_dmg_add, player_hit_pen}
    # P1/P3/P5 carry extra cushion to offset the gear-unlock power spike.
    0: {"hp": 0,  "hit": 0.00, "dmg": 0, "pen": 0.000},
    1: {"hp": 9,  "hit": 0.08, "dmg": 0, "pen": 0.030},   # Obsidian unlock cushion
    2: {"hp": 9,  "hit": 0.09, "dmg": 0, "pen": 0.050},
    3: {"hp": 12, "hit": 0.13, "dmg": 0, "pen": 0.080},   # Frost unlock cushion
    4: {"hp": 14, "hit": 0.15, "dmg": 0, "pen": 0.110},
    5: {"hp": 24, "hit": 0.21, "dmg": 0, "pen": 0.140},   # Void unlock cushion (big)
    6: {"hp": 26, "hit": 0.24, "dmg": 0, "pen": 0.165},
    7: {"hp": 27, "hit": 0.26, "dmg": 1, "pen": 0.190},   # purgatory: only +dmg row
}
PLAYER_HIT_FLOOR: float = 0.05                     # hard floor so Reckless remains playable
PLAYER_HIT_CEILING: float = 0.90                   # hard ceiling — luminosity already eats hit chance, so leave a wider cap
BOSS_FREE_FIGHT_ACCURACY_MOD: float = 0.6          # multiplied into player_hit when wager == 0
BOSS_ROUND_CAP: int = 20                           # safety valve against infinite loops
WIN_CHANCE_CAP: float = 0.95                       # ceiling on displayed/computed win probability
WIN_CHANCE_FLOOR: float = 0.05                     # floor on displayed/computed win probability ("miracle" chance)

# Boss archetypes — applied on top of risk-tier base stats so each boss
# in a tier feels distinct (e.g. Pudge tanks, Lina glass-cannons).
# hp_mult applies to base boss_hp; hit/dmg are additive offsets.
BOSS_ARCHETYPES: dict[str, dict[str, float]] = {
    "tank":         {"hp_mult": 1.5, "hit_offset": -0.03, "dmg_offset": 0},
    "bruiser":      {"hp_mult": 1.0, "hit_offset": 0.00,  "dmg_offset": 0},
    "glass_cannon": {"hp_mult": 0.7, "hit_offset": 0.05,  "dmg_offset": 1},
    "slippery":     {"hp_mult": 0.8, "hit_offset": 0.10,  "dmg_offset": 0},
}

# Per-boss archetype assignment (heuristic by Dota persona).
BOSS_ARCHETYPE_BY_ID: dict[str, str] = {
    # Tier 25
    "grothak":             "bruiser",
    "pudge":               "tank",
    "ogre_magi":           "glass_cannon",
    # Tier 50
    "crystalia":           "bruiser",
    "crystal_maiden":      "glass_cannon",
    "tusk":                "tank",
    # Tier 75
    "magmus_rex":          "tank",
    "lina":                "glass_cannon",
    "doom":                "bruiser",
    # Tier 100
    "void_warden":         "slippery",
    "spectre":             "slippery",
    "void_spirit":         "slippery",
    # Tier 150
    "sporeling_sovereign": "tank",
    "treant_protector":    "tank",
    "broodmother":         "glass_cannon",
    "xalatath":            "slippery",
    # Tier 200
    "chronofrost":         "slippery",
    "faceless_void":       "slippery",
    "weaver":              "slippery",
    "lilith":              "glass_cannon",
    # Tier 275
    "nameless_depth":      "tank",
    "oracle":              "glass_cannon",
    "terrorblade":         "glass_cannon",
    "underlord":           "tank",
}

# Payouts: depth -> (cautious_multiplier, bold_multiplier, reckless_multiplier).
# Flatter and harder than the pre-nerf table; the old exponential growth at
# top-end depths was the main jopacoin inflation source.
BOSS_PAYOUTS: dict[int, tuple[float, float, float]] = {
    25:  (1.5, 2.4, 4.3),
    50:  (1.8, 3.3, 5.8),
    75:  (2.1, 4.3, 7.3),
    100: (2.4, 4.8, 8.4),
    150: (2.7, 5.2, 8.8),
    200: (3.0, 5.8, 9.7),
    275: (3.3, 6.5, 10.3),
    350: (2.5, 3.8, 7.5),
}

# Flat JC every boss victory pays, on top of any wager profit, so a win is
# never empty — the wager-payout taper can otherwise floor a low-risk,
# high-win-chance win at 0. Keyed by boundary depth; the pinnacle (350) uses
# its own PINNACLE_BASE_JC_REWARD instead.
BOSS_VICTORY_BASE_JC: dict[int, int] = {
    25: 15,
    50: 20,
    75: 25,
    100: 30,
    150: 40,
    200: 50,
    275: 65,
}




# Boss Phase 2 Definitions (P2+, Sekiro / Mythic Lura style)
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class BossPhase2Def:
    """Secret second phase for bosses at prestige 2+."""
    depth: int
    name: str
    title: str
    dialogue: list[str]
    win_odds_penalty: float   # additional penalty to win odds (-0.10 = -10%)


BOSS_PHASE2: dict[int, BossPhase2Def] = {
    25: BossPhase2Def(
        depth=25,
        name="Grothak the Undying",
        title="Skeletal Wrath",
        dialogue=[
            "You... thought that would STOP me?! I shed my flesh like a coat!",
            "Back again, and so am I. Bones don't tire, worm.",
            "At this point my chiropractor is just a necromancer.",
        ],
        win_odds_penalty=-0.10,
    ),
    50: BossPhase2Def(
        depth=50,
        name="Crystalia Shattered",
        title="The Thousand Reflections",
        dialogue=[
            "You broke me! But every shard is a NEW me! Geometry is ETERNAL!",
            "Which one is real? Trick question. They ALL are.",
            "I have become a fractal. Please send help.",
        ],
        win_odds_penalty=-0.10,
    ),
    75: BossPhase2Def(
        depth=75,
        name="Magmus Unbound",
        title="The Living Eruption",
        dialogue=[
            "My SHELL was holding me BACK! I AM THE VOLCANO NOW!",
            "Cancel my PTO. This is PERSONAL.",
            "I'm literally just lava in a vaguely angry shape at this point.",
        ],
        win_odds_penalty=-0.10,
    ),
    100: BossPhase2Def(
        depth=100,
        name="The Void Unraveled",
        title="What Lies Beyond Nothing",
        dialogue=[
            "You defeated nothing. I AM nothing. How do you kill nothing?",
            "I un-existed. Now I un-un-exist. The math checks out.",
            "I'm a philosophical problem now. Good luck.",
        ],
        win_odds_penalty=-0.12,
    ),
    150: BossPhase2Def(
        depth=150,
        name="The Sporeling Collective",
        title="We Are Legion",
        dialogue=[
            "You killed one. We are MILLIONS. The mycelium REMEMBERS.",
            "We grew back. We always grow back. That's kind of our thing.",
            "Would you like to become one of us? The benefits are excellent.",
        ],
        win_odds_penalty=-0.12,
    ),
    200: BossPhase2Def(
        depth=200,
        name="Chronofrost Paradox",
        title="The Time That Bites Back",
        dialogue=[
            "You defeated me five minutes ago. I came back to before you did.",
            "This is the 47th time we've done this. You just don't remember.",
            "I've already won. I just haven't told you yet.",
        ],
        win_odds_penalty=-0.15,
    ),
    275: BossPhase2Def(
        depth=275,
        name="The Name Reclaimed",
        title="[DATA EXPUNGED]",
        dialogue=[
            "I remember my name now. It's yours.",
            "We are the same person. I'm just the part you buried.",
            "Take my hand. Let's dig together. Forever.",
        ],
        win_odds_penalty=-0.15,
    ),
}


# ---------------------------------------------------------------------------
# Boss Phase Gates & Phase 3 Definitions (boss revamp)
# ---------------------------------------------------------------------------
# Phase gates control when multi-phase boss fights unlock.
# Phase 2: P2+ on any tier (was P4). Phase 3: P5+ AND tier >= 100. Pinnacle
# is always 3-phase regardless of prestige.
BOSS_PHASES: dict[str, int | bool] = {
    "phase_2_min_prestige": 2,
    "phase_3_min_prestige": 5,
    "phase_3_min_tier": 100,
}


@dataclass(frozen=True)
class BossPhase3Def:
    """Endgame third phase for tier 100+ bosses at prestige 5+."""
    depth: int
    name: str
    title: str
    dialogue: list[str]
    win_odds_penalty: float


BOSS_PHASE3: dict[int, BossPhase3Def] = {
    100: BossPhase3Def(
        depth=100,
        name="The Void Itself",
        title="There Was Never Anything Here",
        dialogue=[
            "You unraveled me. The unraveling unravels in turn. Endless.",
            "There is no third phase. There is no second. There was no first.",
            "You are arguing with the carpet now. Good luck.",
        ],
        win_odds_penalty=-0.15,
    ),
    150: BossPhase3Def(
        depth=150,
        name="The Hivemind Awoken",
        title="Every Spore Speaks At Once",
        dialogue=[
            "We are a chorus. The chorus is a single voice. The voice is many.",
            "You are a single thread in our weave. We have eaten threads before.",
            "Every breath you take is a vote in our favor.",
        ],
        win_odds_penalty=-0.18,
    ),
    200: BossPhase3Def(
        depth=200,
        name="Chronofrost Rewound",
        title="The Loop That Forgot Itself",
        dialogue=[
            "I have already won. I have always already won. The verb is set.",
            "We have done this 1,032 times. You only remember one.",
            "Round three. The clock has unwound to zero.",
        ],
        win_odds_penalty=-0.20,
    ),
    275: BossPhase3Def(
        depth=275,
        name="The Final Erasure",
        title="[REDACTED] [REDACTED] [REDACTED]",
        dialogue=[
            "I take your name. I take your shape. I take the gap where you were.",
            "The depth gets the last word. The depth has always had the last word.",
            "You become a story told by the rocks. Be a good story.",
        ],
        win_odds_penalty=-0.20,
    ),
}


# ---------------------------------------------------------------------------
# Phase Transition Events (boss revamp)
# ---------------------------------------------------------------------------
# Drawn at random when a boss enters Phase 2 or Phase 3. Effects are
# applied to the in-progress encounter; flavor goes into the embed.

@dataclass(frozen=True)
class PhaseTransitionEvent:
    id: str
    flavor: str
    description: str
    # Effects applied to the duel mid-fight. Any unset key has no effect.
    player_hp_delta: int = 0
    boss_hp_delta: int = 0
    player_hit_offset: float = 0.0  # additive to player_hit for rest of fight
    boss_hit_offset: float = 0.0
    player_dmg_delta: int = 0
    boss_dmg_delta: int = 0
    luminosity_delta: int = 0       # one-shot tunnel luminosity adjustment


PHASE_TRANSITION_EVENTS: list[PhaseTransitionEvent] = [
    PhaseTransitionEvent(
        id="cave_in",
        flavor="Stalactites fracture overhead.",
        description="Both you and the boss take 1 HP damage.",
        player_hp_delta=-1, boss_hp_delta=-1,
    ),
    PhaseTransitionEvent(
        id="fissure",
        flavor="A magma fissure opens between you.",
        description="-5% player_hit for the remainder of the fight; -5 luminosity.",
        player_hit_offset=-0.05, luminosity_delta=-5,
    ),
    PhaseTransitionEvent(
        id="glowburst",
        flavor="A vein of phosphorus ignites.",
        description="+20 luminosity but the boss sees you better (+5% boss_hit).",
        boss_hit_offset=0.05, luminosity_delta=20,
    ),
    PhaseTransitionEvent(
        id="cold_snap",
        flavor="Frost sweeps through the chamber.",
        description="Both attacks weaken: -1 player_dmg, -1 boss_dmg.",
        player_dmg_delta=-1, boss_dmg_delta=-1,
    ),
    PhaseTransitionEvent(
        id="spore_cloud",
        flavor="Spores fill the air.",
        description="Sluggishness: -3% player_hit and -3% boss_hit.",
        player_hit_offset=-0.03, boss_hit_offset=-0.03,
    ),
    PhaseTransitionEvent(
        id="void_pull",
        flavor="Reality folds inward.",
        description="Both lose 2 HP.",
        player_hp_delta=-2, boss_hp_delta=-2,
    ),
    PhaseTransitionEvent(
        id="echoing_drip",
        flavor="A drip falls somewhere far below. The sound never lands.",
        description="The dark presses closer. -3 luminosity.",
        luminosity_delta=-3,
    ),
    PhaseTransitionEvent(
        id="iron_tang",
        flavor="The air goes coppery. Something old just opened its eyes.",
        description="The boss strikes truer (+4% boss_hit).",
        boss_hit_offset=0.04,
    ),
    PhaseTransitionEvent(
        id="shifting_walls",
        flavor="The walls breathe in once. They do not breathe out.",
        description="-2 player HP from the squeeze.",
        player_hp_delta=-2,
    ),
    PhaseTransitionEvent(
        id="hollow_tone",
        flavor="A tone hums through the stone — too low to hear, too loud to ignore.",
        description="-2% player_hit, +2% boss_hit.",
        player_hit_offset=-0.02, boss_hit_offset=0.02,
    ),
    PhaseTransitionEvent(
        id="crystal_bloom",
        flavor="Veins of pale crystal bloom across the floor.",
        description="+10 luminosity, +1 player damage from a fragment grabbed mid-fight.",
        luminosity_delta=10, player_dmg_delta=1,
    ),
    PhaseTransitionEvent(
        id="tremor",
        flavor="The mountain shifts on its bones.",
        description="Both fighters stumble: -1 player_dmg, -1 boss_dmg.",
        player_dmg_delta=-1, boss_dmg_delta=-1,
    ),
    PhaseTransitionEvent(
        id="ash_fall",
        flavor="Black ash drifts down from a ceiling that wasn't there before.",
        description="-5 luminosity, -2% player_hit.",
        luminosity_delta=-5, player_hit_offset=-0.02,
    ),
    PhaseTransitionEvent(
        id="quiet",
        flavor="Everything stops. Even the dark holds its breath.",
        description="The pause leaves no mark. The fight resumes.",
    ),
]


# ---------------------------------------------------------------------------
# Pinnacle Boss (boss revamp)
# ---------------------------------------------------------------------------
# A new 8th boss boundary at depth 350 that gates prestige. One of three
# pinnacle candidates is rolled and locked per prestige cycle.
# Always 3 phases. Drops a relic with 2 random rolls on victory.

PINNACLE_DEPTH: int = 350
PINNACLE_RETREAT_FORESHADOW_DEPTH: int = 335  # /dig info hints from this depth
PINNACLE_FORESHADOW_DEPTH: int = 326          # subtle hint after T275 cleared


@dataclass(frozen=True)
class PinnaclePhaseDef:
    """One phase of a pinnacle boss."""
    archetype: str
    title: str
    transition_dialogue: list[str]
    mechanic_pool: tuple[str, ...] = ()


@dataclass(frozen=True)
class PinnacleBossDef:
    """A pinnacle boss candidate. One is rolled and locked per prestige cycle."""
    boss_id: str
    name: str
    persona: str
    ascii_art: str
    phases: tuple[PinnaclePhaseDef, PinnaclePhaseDef, PinnaclePhaseDef]


PINNACLE_BOSSES: dict[str, PinnacleBossDef] = {
    "forgotten_king": PinnacleBossDef(
        boss_id="forgotten_king",
        name="The Forgotten King",
        persona="ancient, dignified, hollowed by time",
        ascii_art=(
            "    .--^^--.\n"
            "   /  ::::  \\\n"
            "  | (o)  (o) |\n"
            "  |    /\\    |\n"
            "  |   '--'   |\n"
            "   \\  '__'  /\n"
            "    '------'\n"
            "      ||||\n"
            "    ##====##"
        ),
        phases=(
            PinnaclePhaseDef(
                archetype="tank",
                title="The Forgotten King",
                transition_dialogue=[],
                mechanic_pool=("king_decree",),
            ),
            PinnaclePhaseDef(
                archetype="glass_cannon",
                title="The Crowned Hunger",
                transition_dialogue=[
                    "The crown burns. I am hungry now. Forgive me.",
                    "Decorum slips. Hunger speaks.",
                ],
                mechanic_pool=("king_feast",),
            ),
            PinnaclePhaseDef(
                archetype="slippery",
                title="The Last Breath of Kings",
                transition_dialogue=[
                    "Last breath. Last lesson. Pay attention.",
                    "I die slowly. You will witness.",
                ],
                mechanic_pool=("king_deathbed",),
            ),
        ),
    ),
    "hollowforged": PinnacleBossDef(
        boss_id="hollowforged",
        name="Hollowforged",
        persona="the depth made flesh, plural, mineral",
        ascii_art=(
            "  /\\/\\/\\/\\/\\/\\\n"
            " /            \\\n"
            "|  __    __    |\n"
            "| (oo)  (oo)   |\n"
            "|              |\n"
            " \\  ========  /\n"
            "  \\__________/\n"
            "    ||    ||\n"
            "   ###    ###"
        ),
        phases=(
            PinnaclePhaseDef(
                archetype="bruiser",
                title="Hollowforged",
                transition_dialogue=[],
                mechanic_pool=("hollow_walls_close",),
            ),
            PinnaclePhaseDef(
                archetype="tank",
                title="Hollowforged Reformed",
                transition_dialogue=[
                    "Reform. The mine has new walls now.",
                    "The walls speak in a different dialect.",
                ],
                mechanic_pool=("hollow_shape_shift",),
            ),
            PinnaclePhaseDef(
                archetype="slippery",
                title="Hollowforged Pluralized",
                transition_dialogue=[
                    "Plural. The depth is many things at once.",
                    "We are the chamber and the wall and the air.",
                ],
                mechanic_pool=("hollow_many_voices",),
            ),
        ),
    ),
    "first_digger": PinnacleBossDef(
        boss_id="first_digger",
        name="The First Digger",
        persona="gaunt, manic, the one who never came back up",
        ascii_art=(
            "       /\\\n"
            "      /  \\\n"
            "     /    \\\n"
            "    | O  O |\n"
            "    |  /\\  |\n"
            "     \\ -- /\n"
            "      \\__/\n"
            "       ||\n"
            "    ___||___\n"
            "   |________|"
        ),
        phases=(
            PinnaclePhaseDef(
                archetype="glass_cannon",
                title="The First Digger",
                transition_dialogue=[],
                mechanic_pool=("digger_pickaxe_duel",),
            ),
            PinnaclePhaseDef(
                archetype="slippery",
                title="The Digger Unbound",
                transition_dialogue=[
                    "Unbound. The pickaxe is no longer needed.",
                    "I dig with my hands now. Cleaner.",
                ],
                mechanic_pool=("digger_phasing",),
            ),
            PinnaclePhaseDef(
                archetype="glass_cannon",
                title="The Digger Eternal",
                transition_dialogue=[
                    "Eternal. The tunnel is me. I am the tunnel.",
                    "Last shift. Last dig. Last.",
                ],
                mechanic_pool=("digger_tunnel_collapse",),
            ),
        ),
    ),
}

PINNACLE_POOL_IDS: tuple[str, ...] = ("forgotten_king", "hollowforged", "first_digger")


# Pinnacle relic — random 2 stats from this pool, name = base + suffix.
@dataclass(frozen=True)
class RelicStatRoll:
    """Possible stat roll for a pinnacle relic. effects keys feed into combat helpers."""
    id: str
    label: str
    effects: dict   # e.g. {"player_hp_bonus": 1} or {"jc_multiplier": 0.05}


PINNACLE_RELIC_STAT_POOL: tuple[RelicStatRoll, ...] = (
    # Combat
    RelicStatRoll("hp_plus_1",        "Tougher skin",
                  {"player_hp_bonus": 1}),
    RelicStatRoll("hit_plus_002",     "Steadier hands",
                  {"player_hit_bonus": 0.02}),
    RelicStatRoll("dmg_plus_per_100", "Stronger with depth",
                  {"player_dmg_per_100_depth": 1}),
    RelicStatRoll("boss_hit_minus",   "Bosses miss more often",
                  {"boss_hit_offset": -0.02}),
    RelicStatRoll("boss_hp_minus_10", "Bosses arrive weakened",
                  {"boss_hp_multiplier": -0.10}),
    RelicStatRoll("boss_payout_5",    "Bosses pay better",
                  {"boss_payout_bonus": 0.05}),
    # Dig
    RelicStatRoll("jc_plus_5",        "Richer veins",
                  {"jc_multiplier": 0.05}),
    RelicStatRoll("cave_in_minus_5",  "Steadier ceilings",
                  {"cave_in_reduction": 0.05}),
    RelicStatRoll("lum_refill_2",     "Brighter mornings",
                  {"lum_refill_bonus": 2}),
    RelicStatRoll("durability_minus", "Gear lasts longer",
                  {"durability_reduction": 0.10}),
    RelicStatRoll("inventory_plus_1", "Roomier pack",
                  {"inventory_bonus": 1}),
    # Utility
    RelicStatRoll("streak_immunity",  "Streak persists, once per delve",
                  {"streak_immunity": True}),
    RelicStatRoll("extra_relic_slot", "Another relic finds room",
                  {"relic_slot_bonus": 1}),
    RelicStatRoll("scout_free",       "Scouting comes cheap",
                  {"scout_free": True}),
    RelicStatRoll("cheer_buff",       "Cheers ring louder",
                  {"cheer_bonus": 0.01}),
)

PINNACLE_RELIC_SUFFIX_POOL: tuple[str, ...] = (
    "Echoes", "Hunger", "Patience", "Ruin", "Bloom",
    "Silence", "Endings", "First Light", "Last Breath", "Hollow",
    "Persistence", "Forgotten Things",
)

PINNACLE_RELIC_BASE_NAME: dict[str, str] = {
    "forgotten_king": "Crown",
    "hollowforged":   "Heart",
    "first_digger":   "Pickaxe",
}

# Flat JC reward layered on top of the relic drop.
PINNACLE_BASE_JC_REWARD: int = 500
PINNACLE_JC_PER_PRESTIGE: int = 100


_RELIC_NAME_BY_ID: dict[str, str] = {r.id: r.name for r in RELICS}
_PINNACLE_STAT_LABEL_BY_ID: dict[str, str] = {
    s.id: s.label for s in PINNACLE_RELIC_STAT_POOL
}


def format_relic_label(artifact_id: str, *, with_stats: bool = False) -> str:
    """Render a relic's display name from its stored artifact_id.

    Plain relics resolve via the RELICS registry. Pinnacle ids of the form
    ``pinnacle:<base>:<suffix>:<stat1>:<stat2>`` are parsed into
    ``"<base> of <suffix>"``; with ``with_stats=True`` the recognized stat
    labels are appended in parens.
    """
    if not artifact_id:
        return "?"
    if artifact_id.startswith("pinnacle:"):
        parts = artifact_id.split(":")
        if len(parts) >= 3:
            base = parts[1]
            suffix = parts[2]
            label = f"{base} of {suffix}"
            if with_stats:
                stat_labels = [
                    _PINNACLE_STAT_LABEL_BY_ID[sid]
                    for sid in parts[3:]
                    if sid in _PINNACLE_STAT_LABEL_BY_ID
                ]
                if stat_labels:
                    label += f" ({', '.join(stat_labels)})"
            return label
        return artifact_id
    return _RELIC_NAME_BY_ID.get(artifact_id, artifact_id)


# ---------------------------------------------------------------------------
# Retreat Cost (boss revamp)
# ---------------------------------------------------------------------------
# Retreat costs depth blocks.
RETREAT_BLOCK_LOSS_MIN: int = 2
RETREAT_BLOCK_LOSS_MAX: int = 3


# ---------------------------------------------------------------------------
# Persisted boss HP / regen (boss revamp)
# ---------------------------------------------------------------------------
# Slowed to "1 HP per 3 hours" so soften-and-retreat damage actually sticks
# between fights — under the prior 1/2h rate a single dig-cooldown gap was
# already regenning half of a hard-won softening.
BOSS_HP_REGEN_PER_3_HOURS: int = 1




# ---------------------------------------------------------------------------
# Boss Dialogue V2 (boss revamp, pre-generated)
# ---------------------------------------------------------------------------
# Per-boss dialogue keyed by slot:
#   first_meet: line on first encounter this delve (resets on prestige)
#   after_defeat: last fight player won (boss may have been weakened)
#   after_retreat: last fight player retreated
#   after_close_win: last fight player won with low win-prob (<0.6)
#   after_scout: last action was scout
# Tokens are substituted at render time:
#   {streak}, {depth}, {prestige}, {killed_boss_name}.
BOSS_DIALOGUE_V2: dict[str, dict[str, list[str]]] = {
    # ---- Tier 25 -------------------------------------------------------
    "grothak": {
        "first_meet": [
            "I have stood here longer than you have been alive. Continue.",
            "You came down. Most go up. I respect this.",
            "I am Grothak. You are not. Begin.",
        ],
        "after_defeat": [
            "Again. I will not break. You might.",
            "Streak {streak} days, you say? I have stood here {streak} centuries.",
            "Round two. I have weight. You have intent.",
        ],
        "after_retreat": [
            "You left. The stone remembers.",
            "Patience. I am not hard to find.",
            "You will be back. They always are.",
        ],
        "after_close_win": [
            "A chip is not a crack. Try again.",
            "You bled me. Acceptable. Not enough.",
            "I felt that. I have not felt that in some time.",
        ],
        "after_scout": [
            "Looking? Look. I am unbothered.",
            "Note my stance. It will not change.",
        ],
    },
    "pudge": {
        "first_meet": [
            "You look stringy. Maybe with sauce.",
            "Fresh meat. Don't run. You'll just sweat.",
            "I haven't eaten today. You'll do.",
        ],
        "after_defeat": [
            "You! ...have you been working out?",
            "Lucky. The hook was wet.",
            "Round two. Bring friends. Or don't.",
        ],
        "after_retreat": [
            "Run! It only adds flavor.",
            "Coward soup. My favorite.",
            "Smart. I'd run from me too.",
        ],
        "after_close_win": [
            "Scratched. You're learning.",
            "Bleeding? Both of us. Cute.",
            "I almost had you. Almost.",
        ],
        "after_scout": [
            "Watching me eat? Weirdo.",
            "Take notes. There's a quiz.",
        ],
    },
    "ogre_magi": {
        "first_meet": [
            "Hi! ...wait, who are you?",
            "Left head says fight. Right head says snacks. Compromise: fight snack.",
            "FIRE! ...what was I doing?",
        ],
        "after_defeat": [
            "We saw you yesterday. We forgot you today. Hi again!",
            "Did you win last time? Don't tell us. We don't believe you.",
            "Streak {streak} days! Both heads agree we hate that.",
        ],
        "after_retreat": [
            "You ran! Or arrived! Hard to say!",
            "Goodbye! Or hello! Same thing!",
            "We won? We didn't win? Doesn't matter, FIRE!",
        ],
        "after_close_win": [
            "Multicast: ow ow ow.",
            "We meant to do that. Both heads agree. Probably.",
            "You're hard to forget. We'll work on it.",
        ],
        "after_scout": [
            "You're staring. We like staring. STARE BACK.",
            "Two heads, two opinions on you. Both bad.",
        ],
    },
    # ---- Tier 50 -------------------------------------------------------
    "crystalia": {
        "first_meet": [
            "Do you see how the light loves me? It will not love you.",
            "I have a thousand faces. None of them like yours.",
            "Approach. Watch yourself approach. Watch yourself approach. Watch yourself—",
        ],
        "after_defeat": [
            "You chipped me. The chip is more beautiful than you.",
            "Refracted again. The mirror reverses. So will I.",
            "{streak} days of digging and you bring this light to me. Tasteless.",
        ],
        "after_retreat": [
            "Run. The crystal reflects everything, including the back of your head.",
            "Half a hundred faces watched you flee. They will gossip.",
            "You return to surface daylight. I pity you.",
        ],
        "after_close_win": [
            "A facet broken. I have nine hundred and ninety-nine others.",
            "Light bleeds. I bleed. Cute symmetry.",
            "You're sharper than I thought. Not as sharp as me.",
        ],
        "after_scout": [
            "Gawker. Make a wish.",
            "I see you in fragments. Most of them are unflattering.",
        ],
    },
    "crystal_maiden": {
        "first_meet": [
            "Stand still. The cold finds the still.",
            "I'm small. The fields I cast are not.",
            "Wave hello. It'll be the last time you wave with both arms.",
        ],
        "after_defeat": [
            "You melted me. Rude. I'll re-form by Tuesday.",
            "I'll remember the warmth of your win. Briefly.",
            "Round two. Bring a coat.",
        ],
        "after_retreat": [
            "Run. I'll catch up. Frost is patient.",
            "Goodbye. The glaciers I made are still here.",
            "Coward! ...wait, sensible? I respect both.",
        ],
        "after_close_win": [
            "You felt the field! Now you're afraid.",
            "Survived? Lucky. The cold remembers your gait.",
            "Almost. Almost is colder than 'no'.",
        ],
        "after_scout": [
            "Studying my robes? They're insulated. Yours aren't.",
            "Don't blink. I freeze the eyelashes first.",
        ],
    },
    "tusk": {
        "first_meet": [
            "WALRUS PUNCH WARM-UP! You'll do as the tackle dummy.",
            "Hahaha! Fresh blood for the snowfield.",
            "You came down here in those? Bold. Stupid. I respect it.",
        ],
        "after_defeat": [
            "Round two! I've packed harder snowballs!",
            "{streak} days of digging and you've still got soft hands. Cute.",
            "You won. I respect winners. Now eat snow.",
        ],
        "after_retreat": [
            "Run! I'll roll downhill after you!",
            "Cold feet, eh? Mine never get cold.",
            "Tusk waits. Tusk is patient. Tusk is also bored.",
        ],
        "after_close_win": [
            "You took the punch! You stood up! Mostly!",
            "Bruised but proud. That's the way.",
            "I felt that. Want to feel mine?",
        ],
        "after_scout": [
            "Squinting? My armor is thick. Your eyes are not.",
            "Ho ho! A scout! Be sure to scout the fist.",
        ],
    },
    # ---- Tier 75 -------------------------------------------------------
    "magmus_rex": {
        "first_meet": [
            "Bow or burn. Either is acceptable.",
            "You bring iron into a furnace. Charming.",
            "I have been king longer than your line has had names.",
        ],
        "after_defeat": [
            "You scorched me. The throne has a new dent.",
            "Round two. The crown is heavier this time.",
            "Streak {streak} days and still you crawl back. Persistent rats are still rats.",
        ],
        "after_retreat": [
            "Withdraw. The lava has memory. So do I.",
            "Hot under your collar? Try mine.",
            "You will be back. The mantle pulls everything down eventually.",
        ],
        "after_close_win": [
            "A spark off my crown. The crown remains.",
            "Embers. You are bringing me embers. Adorable.",
            "I felt warmth. Strange — I am warmth.",
        ],
        "after_scout": [
            "Look. I am unconcerned. Look longer if you wish.",
            "Inspect my regalia. It survives diggers like you.",
        ],
    },
    "lina": {
        "first_meet": [
            "I've been waiting. Don't bore me.",
            "You're early. I haven't finished applying my eyeliner.",
            "Make this fun. Make this fast. Pick one.",
        ],
        "after_defeat": [
            "You won? Let me check. ...rude.",
            "Defeated by depth-{depth} dirt. The shame.",
            "Round two. I'm bringing the dragon this time.",
        ],
        "after_retreat": [
            "Run! I'll burn brighter while you're gone!",
            "Goodbye. Take your retreat with a side of fire.",
            "Patience is a fuel. I have plenty.",
        ],
        "after_close_win": [
            "Singed but standing. Cute outfit, by the way.",
            "Almost combusted. I respect almost.",
            "You'll need ointment. I have a recommendation.",
        ],
        "after_scout": [
            "Watch closely. The next one is faster.",
            "Don't blink. The flash blinds easily.",
        ],
    },
    "doom": {
        "first_meet": [
            "Hello. I am Doom. Goodbye.",
            "Your name. I'd like it for the list.",
            "You will burn. I'll wait while you process this.",
        ],
        "after_defeat": [
            "Last round you survived. This round, less likely.",
            "Mark renewed. Streak {streak} noted in the ledger.",
            "I underestimated you. I will adjust.",
        ],
        "after_retreat": [
            "Branded. You carry me with you now.",
            "Run. The mark catches up.",
            "You will return. Branded things always do.",
        ],
        "after_close_win": [
            "A scratch. The brand still burns under it.",
            "Almost. The list is patient.",
            "You bleed neatly. I appreciate that.",
        ],
        "after_scout": [
            "Look. The brand is patient.",
            "Memorize my face. It will be the last polite one you see.",
        ],
    },
    # ---- Tier 100 ------------------------------------------------------
    "void_warden": {
        "first_meet": [
            "I am between two thoughts. You arrived in the gap.",
            "Hello. Or have we already had this conversation. I forget the order.",
            "Step closer. Or further. The geometry is forgiving.",
        ],
        "after_defeat": [
            "I lost. Or I lost. Or I will lose. The verbs blur.",
            "Streak {streak} days. The streak is also a line. Lines fold.",
            "You won. The previous you won. The next you may not.",
        ],
        "after_retreat": [
            "You retreat. Ahead, behind. Same direction here.",
            "The void does not chase. It anticipates.",
            "Goodbye. Or hello in another moment.",
        ],
        "after_close_win": [
            "You bled the right amount. Coincidence is generous.",
            "I admit confusion. I admit it backwards too.",
            "Close. Closer than the math allowed.",
        ],
        "after_scout": [
            "You watch. I am also watching. We have always been watching.",
            "The geometry approves of your inspection. The Warden does not.",
        ],
    },
    "spectre": {
        "first_meet": [
            "...",
            "(The shade does not greet. It haunts.)",
            "You step into a doorway you did not see. There is no door.",
        ],
        "after_defeat": [
            "You ended me. I have been ended before. It does not stop me long.",
            "Streak {streak} days. You haunt the depths. So do I.",
            "Vengeance has been delayed. Not denied.",
        ],
        "after_retreat": [
            "You leave. I am already with you.",
            "Footsteps fade. Mine do not.",
            "The shade always follows.",
        ],
        "after_close_win": [
            "Surprised? I bleed shadow.",
            "Almost yours. Almost mine.",
            "A near miss. I have eternity.",
        ],
        "after_scout": [
            "Look closer. There is more of me than you see.",
            "I am behind you. And in front. Pick.",
        ],
    },
    "void_spirit": {
        "first_meet": [
            "Hi! Or — wait, is it 'bye'? Always confuses me.",
            "I'm the echo. The original is busy.",
            "Stand still! Or don't. Either way I'll be where you aren't.",
        ],
        "after_defeat": [
            "Caught me. The original will be embarrassed.",
            "{streak} days digging and you found a glitch. Nice.",
            "Score: you 1, the lattice 0. The lattice is stubborn.",
        ],
        "after_retreat": [
            "Bye! See you in the next chamber!",
            "You're rotating, but I'm rotating faster.",
            "Goodbye! Or hello! Both! Neither!",
        ],
        "after_close_win": [
            "Scratched the lattice. The lattice does not forget.",
            "Almost phased through me. Almost.",
            "I felt your edge. Mostly through your edge.",
        ],
        "after_scout": [
            "Hello, the watcher! Here, here, here, here.",
            "Pick a me. They're all valid.",
        ],
    },
    # ---- Tier 150 ------------------------------------------------------
    "sporeling_sovereign": {
        "first_meet": [
            "We are many. You are alone. We forgive this.",
            "Welcome to the bloom. Mind the spores. They mind you.",
            "Approach. The mycelium catalogs you.",
        ],
        "after_defeat": [
            "You harvested me. The spores remember harvest.",
            "Round two. We have re-bloomed in your absence.",
            "Streak {streak} days. We are a streak too. Older.",
        ],
        "after_retreat": [
            "Leave. We are also outside, in places you have walked.",
            "Goodbye. You take spores with you.",
            "Retreat, watered properly, becomes a return.",
        ],
        "after_close_win": [
            "Bruised the bloom. The bloom is patient.",
            "A petal lost. We have many petals.",
            "Closer than expected. We will adjust.",
        ],
        "after_scout": [
            "Watch the bloom. The bloom watches back.",
            "Inspect the spore-clouds. They take notes.",
        ],
    },
    "treant_protector": {
        "first_meet": [
            "Little digger. Why so deep?",
            "The roots heard you coming. Patience, child.",
            "Welcome. Try not to chop anything.",
        ],
        "after_defeat": [
            "You bested an elder. I am surprised. And amused.",
            "{streak} days underground and still strong. The sun would suit you.",
            "Round two. The grove has fed.",
        ],
        "after_retreat": [
            "Go. The grove is patient. Trees outlast.",
            "Roots remember. They are still under your boots.",
            "Return whenever. The grove will be here.",
        ],
        "after_close_win": [
            "A leaf fell. I have many leaves.",
            "You scraped bark. Bark grows back.",
            "Closer than I expected. Charmed.",
        ],
        "after_scout": [
            "Look. The grove is unchanged. Mostly.",
            "Examine the rings. There are many, like your scars.",
        ],
    },
    "broodmother": {
        "first_meet": [
            "Welcome, dear. The nest is a little sticky today.",
            "So small. So protein-rich.",
            "Don't mind the children. They mind themselves.",
        ],
        "after_defeat": [
            "You broke a thread. The web has many.",
            "Streak {streak}? Impressive. The children would like to study you. Closely.",
            "Round two. We are hungrier.",
        ],
        "after_retreat": [
            "Go. The web is sticky. You'll bring some with you.",
            "Bye-bye, little dinner. Tell your friends.",
            "Run. The little ones love a chase.",
        ],
        "after_close_win": [
            "Bit me, did you? Cheeky.",
            "A leg lost. I have eight. Plenty.",
            "Closer than we anticipated. The children are impressed.",
        ],
        "after_scout": [
            "Watch the nest. The nest watches you.",
            "Counting eggs? Don't. It's rude.",
        ],
    },
    # ---- Tier 200 ------------------------------------------------------
    "chronofrost": {
        "first_meet": [
            "I have been in this exact second for some time.",
            "You arrive. The second arrives also. They are the same.",
            "Welcome. The fight has already started, technically.",
        ],
        "after_defeat": [
            "You won at second 0.347. I have logged it.",
            "Streak {streak} days. I have streaks too. Mine are colder.",
            "Round two. The same second, refreshed.",
        ],
        "after_retreat": [
            "Leave. I am still in the second. I will be when you return.",
            "Time accommodates retreat. Time also accommodates pursuit.",
            "Goodbye. I will not move. I will not need to.",
        ],
        "after_close_win": [
            "A close second. Pun intended.",
            "You scratched 0.001 of me. The other 0.999 disagrees.",
            "Almost. The clock froze on 'almost'.",
        ],
        "after_scout": [
            "Observe. I am still. You are not.",
            "Take your time. I have all of it.",
        ],
    },
    "faceless_void": {
        "first_meet": [
            "...",
            "(The Timeless does not greet. The Timeless arrives.)",
            "You step into a stopped second. Adjust.",
        ],
        "after_defeat": [
            "You found a gap in the chronosphere. I will close it.",
            "{streak} days. A streak is a kind of timeline. I cut timelines.",
            "Round two. The clock will not be merciful.",
        ],
        "after_retreat": [
            "Backtrack. I do that for a living.",
            "Leave. The chronosphere closes anyway.",
            "Goodbye. Time does not chase. Time waits.",
        ],
        "after_close_win": [
            "Scratched. The damage is in the past now. Both pasts.",
            "Closer than the math. The math will adjust.",
            "Almost. Almost is its own dimension.",
        ],
        "after_scout": [
            "Look. The Timeless is unmoved.",
            "Watch closely. I will not blink because I will not.",
        ],
    },
    "weaver": {
        "first_meet": [
            "Stitch stitch stitch. You arrived in my pattern.",
            "Hi. Little weft, little warp, little you.",
            "The thread says you are interesting. I disagree.",
        ],
        "after_defeat": [
            "Pulled a stitch out of me! Naughty digger!",
            "Streak {streak} days. I have woven {streak} layers around your tunnel.",
            "Round two. The pattern has more knots now.",
        ],
        "after_retreat": [
            "Run! The thread comes with you! Pull, pull!",
            "Goodbye! Or hello, depending on which thread you take!",
            "Bye! I will be in another moment. Always am.",
        ],
        "after_close_win": [
            "A close weave. The pattern shivered.",
            "You almost fell out of time. You still might.",
            "Stitched yourself up. Cute. Mine looks like spaghetti.",
        ],
        "after_scout": [
            "Watching the threads? They watch back. They gossip.",
            "Don't pull on any. You'll regret which one.",
        ],
    },
    # ---- Tier 275 ------------------------------------------------------
    "nameless_depth": {
        "first_meet": [
            "I have no name. You will not provide one.",
            "Approach. Names fall off here.",
            "(Silence so heavy it weighs on your tongue.)",
        ],
        "after_defeat": [
            "You won. The verb does not survive me. Neither will the noun.",
            "Streak {streak}. The number is also forgotten now.",
            "Round two. The silence is louder.",
        ],
        "after_retreat": [
            "You retreat. The depth follows in your unspoken thoughts.",
            "Leave. The Nameless does not pursue. The Nameless waits.",
            "Goodbye. The word departs. The depth remains.",
        ],
        "after_close_win": [
            "A close end. Ends are my specialty.",
            "Almost the bottom. Almost is also a depth.",
            "You bled. The bleeding has no name either.",
        ],
        "after_scout": [
            "Look. There is nothing to see. Look longer.",
            "Inspect the silence. The silence inspects you.",
        ],
    },
    "oracle": {
        "first_meet": [
            "Hello. I knew you'd say nothing back. Disappointed but not surprised.",
            "You arrive. The omen said depth-{depth}. The omen is annoying.",
            "Sit. Not there. There. Yes. The vision said so.",
        ],
        "after_defeat": [
            "You won. Yes. I told myself.",
            "{streak} days of digging. The tea leaves predicted exactly that. Or nothing. Or both.",
            "Round two. I will lose differently this time.",
        ],
        "after_retreat": [
            "You leave. I saw this. Twice. Once with feeling.",
            "Goodbye. The omens are also leaving.",
            "Retreat. Foretold. Boring.",
        ],
        "after_close_win": [
            "Close. The vision said 'close'. Annoyingly accurate.",
            "Bled. The omens warned me. I ignored them.",
            "Almost. The omens warn me of all almosts.",
        ],
        "after_scout": [
            "Stare. I can't see. You're staring at the blindfold.",
            "Watching me? The blindfold watches you. Don't blink.",
        ],
    },
    "terrorblade": {
        "first_meet": [
            "Kneel. The throne is gone. The protocol remains.",
            "You disturb royalty. Royalty disapproves.",
            "I was a prince. Now I am a problem.",
        ],
        "after_defeat": [
            "You unmade my unmaking. The math is poetry.",
            "Streak {streak}. The crown survived longer.",
            "Round two. The illusions will be unkinder.",
        ],
        "after_retreat": [
            "Run. A prince is patient about pursuits.",
            "Goodbye. The illusion of you is still here, mocking.",
            "Coward, by my definition. By yours, sensible.",
        ],
        "after_close_win": [
            "A near-sundering. Of me, this time.",
            "Closer than I dignify.",
            "Almost. The almost has a kind of beauty.",
        ],
        "after_scout": [
            "Watching? The illusions also watch. They are catty.",
            "Inspect closely. Royalty rewards attention.",
        ],
    },
    # ---- Late-prestige additions (only appear at prestige>=3) ----------
    "xalatath": {
        "first_meet": [
            "I have been listening. You came down. I knew.",
            "Quiet, please. Let me hear which one you are.",
            "Set down what you are holding. Not the pick. The other thing.",
        ],
        "after_defeat": [
            "You ended me. The whisper continues. It is not mine alone.",
            "Streak {streak}. The whisper counts too. Differently.",
            "Round two. I have been carrying it longer this time.",
        ],
        "after_retreat": [
            "Climb. The whisper climbs faster.",
            "Go. Take it with you. You won't notice it for a while.",
            "Retreat is also a kind of listening. Welcome to it.",
        ],
        "after_close_win": [
            "A near-naming. Of you, this time.",
            "Almost. The almost is its own word.",
            "Closer than I expected. I am rarely surprised.",
        ],
        "after_scout": [
            "Look. The looking is heard, too.",
            "Examine me. I am examining you back, somewhere quieter.",
        ],
    },
    "lilith": {
        "first_meet": [
            "Mother of suffering, mother of stars. I prefer the first title.",
            "You came to bleed in front of me. Polite of you.",
            "Hatred is a long room. You are at the door. Come in.",
        ],
        "after_defeat": [
            "You ended a Daughter. Hatred raises another.",
            "Streak {streak}. I count in centuries. You do well.",
            "Round two. The hatred has practiced.",
        ],
        "after_retreat": [
            "Leave. Hatred is patient. It learned that from waiting.",
            "Go. I will be here, in this exact configuration.",
            "Retreat is permitted. Hatred forbids forgetting.",
        ],
        "after_close_win": [
            "A near-bleeding. Of me, this time. Curious.",
            "Closer than the cycle allows. Hatred took notes.",
            "Almost. The almost is a sacrament.",
        ],
        "after_scout": [
            "Look. Hatred enjoys attention.",
            "Examine me. I am older than the eye that does it.",
        ],
    },
    "underlord": {
        "first_meet": [
            "Stand. Or kneel. Doesn't matter. We start either way.",
            "Surface dweller. Down here a long time? Doesn't show.",
            "Pit's open. Step in or fight. I'm patient about neither.",
        ],
        "after_defeat": [
            "You won. Earned. Don't rub it in.",
            "Streak {streak}. Respectable. Now don't waste it.",
            "Round two. I sharpened things.",
        ],
        "after_retreat": [
            "Climb out, then. Pit'll wait.",
            "Go. The walk back is its own punishment.",
            "Retreat. Tactical. I'd do the same. Wouldn't.",
        ],
        "after_close_win": [
            "Close one. I wasn't paying attention.",
            "Nearly fell in. Of the two of us, I should not have.",
            "Almost. The almost stings worse than losing.",
        ],
        "after_scout": [
            "Look me over. I'm not a puzzle.",
            "Inspect away. Pit's not going anywhere.",
        ],
    },
    # ---- Pinnacle pool (depth 350) -------------------------------------
    "forgotten_king": {
        "first_meet": [
            "Hello, child. You have walked far. Sit. No, stand. I forget which is the etiquette.",
            "I am a king without a kingdom. You are a digger without an end. We are family of a kind.",
            "Welcome to the throne. The throne is also the bottom of the mine. They are the same room.",
        ],
        "after_defeat": [
            "You ended a king. Streak {streak} of kings, perhaps.",
            "I lost. Royalty does not lose, except when it does.",
            "Round two. The crown is on tighter.",
        ],
        "after_retreat": [
            "You leave royalty mid-audience. Bold.",
            "Go. I will resume my soliloquy.",
            "The court does not chase. The court endures.",
        ],
        "after_close_win": [
            "A close court. The protocol wavered.",
            "You drew royal blood. Rare honor. Rude.",
            "Closer than I have come to ending in some time.",
        ],
        "after_scout": [
            "Inspect the throne. It is a chair. It is also a tomb.",
            "Look at me. The crown does not enjoy attention. I do.",
        ],
    },
    "hollowforged": {
        "first_meet": [
            "We are the mine. The mine has decided to talk back.",
            "You dig us. We dig you back.",
            "Welcome, surface thing. The walls have an opinion.",
        ],
        "after_defeat": [
            "You broke a wall. The wall reforms. The wall is patient.",
            "Round two. We have more walls.",
            "Streak {streak}. Walls also have streaks. Geological ones.",
        ],
        "after_retreat": [
            "Leave. The walls follow. They are slow but committed.",
            "Goodbye. The mine is endless. You will be back.",
            "Retreat is dug too. Welcome.",
        ],
        "after_close_win": [
            "Cracked, not collapsed.",
            "A near-cave-in. We respect the geometry.",
            "Almost. Almost is also a layer.",
        ],
        "after_scout": [
            "Examine the walls. The walls examine you back.",
            "Inspect the rocks. They are taking a head count.",
        ],
    },
    "first_digger": {
        "first_meet": [
            "Oh! Another one! Hello! Don't go up. Don't ever go up.",
            "I started this tunnel. You're in it. Lovely.",
            "First time, eh? Mine was a Tuesday. Long Tuesday.",
        ],
        "after_defeat": [
            "You won. I lost. Wait. Did I want to lose?",
            "{streak} days. Pretender. I have {streak} centuries.",
            "Round two. I dug while you slept.",
        ],
        "after_retreat": [
            "Going up? Don't. The light is wrong now.",
            "Goodbye. The tunnel is mine when you leave it.",
            "Retreat? I retreated once. Then I dug here. Look how that turned out.",
        ],
        "after_close_win": [
            "A close one. The pickaxe is hungry.",
            "Almost. Almost is the depth I prefer.",
            "Closer than I've been to surface in centuries.",
        ],
        "after_scout": [
            "Watching me dig? Take notes. Mostly: don't.",
            "Inspect. Yes. Inspect the hole. The hole inspects you.",
        ],
    },
}


# Subtle pinnacle foreshadowing lines for /dig info, post-T275 clear.
PINNACLE_FORESHADOW_LINES: tuple[str, ...] = (
    "Something stirs below.",
    "The dark hums in a frequency you can almost hear.",
    "A pressure builds in the rock ahead.",
    "Your lantern flame leans, like wind — but there is no wind.",
)
