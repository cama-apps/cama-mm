"""Tunnel-name word pools, layer ASCII art, and ominous name pool for /dig.

Extracted from the original ``dig_constants`` module; see
``services.dig_constants`` for the public facade.
"""

from __future__ import annotations

# ---------------------------------------------------------------------------
# Tunnel Name Word Pools
# ---------------------------------------------------------------------------

TUNNEL_NAME_ADJECTIVES: list[str] = [
    "Whispering", "Echoing", "Forgotten", "Shimmering", "Crooked",
    "Haunted", "Dusty", "Verdant", "Frostbitten", "Soggy",
    "Screaming", "Gilded", "Moldy", "Thundering", "Slippery",
]

TUNNEL_NAME_NOUNS: list[str] = [
    "Descent", "Passage", "Burrow", "Excavation", "Shaft",
    "Tunnel", "Grotto", "Hollow", "Delve", "Pit",
    "Crevice", "Gallery", "Abyss", "Warren", "Mine",
]

TUNNEL_NAME_TITLE_X: list[str] = [
    "Shaft", "Tunnel", "Mine", "Passage", "Depths",
    "Burrow", "Pit", "Grotto", "Hollow", "Excavation",
]

TUNNEL_NAME_TITLE_Y: list[str] = [
    "Sorrows", "Echoes", "Fortune", "Doom", "Whispers",
    "Secrets", "Madness", "Riches", "Despair", "Wonder",
    "Cheese", "Bones", "Regret",
]

TUNNEL_NAME_SILLY: list[str] = [
    "Tunnel McTunnelface",
    "The Big Hole",
    "Definitely Not a Grave",
    "Hole-y Moley",
    "Rock Bottom",
    "Dig Dug's Revenge",
    "The Mole Hole",
    "Shovel Knight's Disgrace",
    "Spelunky Rejects",
    "The Underground Railroad to Nowhere",
]


# ---------------------------------------------------------------------------
# ASCII Art Templates (one per layer, compact for Discord embeds)
# ---------------------------------------------------------------------------

ASCII_ART: dict[str, str] = {
    "Dirt": (
        "  ~~~~~ SURFACE ~~~~~\n"
        "  ||||||||||||||||||||\n"
        "  ====================\n"
        "  .:. dirt .:. dirt .:.\n"
        "  . . . . . . . . . .\n"
        "  .:. . .:. . .:. . .\n"
        "  . . .worms. . . . .\n"
        "  .:. . .:. . .:. . .\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ===================="
    ),
    "Stone": (
        "  ---- dirt above ----\n"
        "  ####################\n"
        "  #  STONE  LAYER   #\n"
        "  # ite ite ite ite #\n"
        "  #  []  []  []  [] #\n"
        "  # gran gran gran  #\n"
        "  #  []  []  []  [] #\n"
        "  #  fossils here   #\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ####################"
    ),
    "Crystal": (
        "  ---- stone above ---\n"
        "  ********************\n"
        "  * CRYSTAL  CAVERNS *\n"
        "  *  /\\  <>  /\\  <>  *\n"
        "  * <>  /\\  <>  /\\   *\n"
        "  *  *sparkle*  <>   *\n"
        "  * /\\  <>  /\\  <>   *\n"
        "  *  <>  /\\  <>  /\\  *\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ********************"
    ),
    "Magma": (
        "  -- crystals above --\n"
        "  ~~~~~~~~~~~~~~~~~~~~\n"
        "  ~  MAGMA  DEPTHS  ~\n"
        "  ~ =/\\/\\= =/\\/\\=  ~\n"
        "  ~  lava   lava    ~\n"
        "  ~ =/\\/\\= =/\\/\\=  ~\n"
        "  ~  *hiss* *glow*  ~\n"
        "  ~ =/\\/\\= =/\\/\\=  ~\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ~~~~~~~~~~~~~~~~~~~~"
    ),
    "Abyss": (
        "  --- magma above ----\n"
        "  .                  .\n"
        "  .   T H E          .\n"
        "  .     A B Y S S    .\n"
        "  .                  .\n"
        "  .   . . . . . .   .\n"
        "  .  nothing here   .\n"
        "  .    ...or is it? .\n"
        "      ⛏️ YOU ARE HERE\n"
        "  .                  ."
    ),
    "Fungal Depths": (
        "  --- abyss above ----\n"
        "  ~~~~~~~~~~~~~~~~~~~~\n"
        "  ~ FUNGAL  DEPTHS  ~\n"
        "  ~ 🍄 .  🍄 .  🍄 ~\n"
        "  ~  . glow .  glow ~\n"
        "  ~ 🍄 .  🍄 .  🍄 ~\n"
        "  ~  spores  drift  ~\n"
        "  ~ 🍄 .  🍄 .  🍄 ~\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ~~~~~~~~~~~~~~~~~~~~"
    ),
    "Frozen Core": (
        "  --- fungal above ---\n"
        "  ********************\n"
        "  *  FROZEN   CORE  *\n"
        "  * ❄️  .  ❄️  .  ❄️ *\n"
        "  *  time  slows    *\n"
        "  * ❄️  .  ❄️  .  ❄️ *\n"
        "  *  frost  creeps  *\n"
        "  * ❄️  .  ❄️  .  ❄️ *\n"
        "      ⛏️ YOU ARE HERE\n"
        "  ********************"
    ),
    "The Hollow": (
        "  --- frozen above ---\n"
        "                      \n"
        "                      \n"
        "     T H E            \n"
        "       H O L L O W    \n"
        "                      \n"
        "    the mine           \n"
        "      remembers you   \n"
        "      ⛏️ YOU ARE HERE\n"
        "                      "
    ),
}



OMINOUS_TUNNEL_NAMES: list[str] = [
    "The Descent That Never Ends",
    "Tomb of the Last Digger",
    "WHERE ARE YOU GOING",
    "The Walls Are Watching",
    "it knows your name",
    "Tunnel of Regret",
    "The Hungry Dark",
    "Something Lives Here",
]

