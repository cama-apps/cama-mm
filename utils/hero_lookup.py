"""
Hero ID to name lookup utility.
"""

import json
import os

# Load hero data from JSON file
_HEROES: dict[str, str] = {}
_HEROES_LOADED = False


def _load_heroes():
    """Load heroes from JSON file."""
    global _HEROES, _HEROES_LOADED
    if _HEROES_LOADED:
        return

    hero_file = os.path.join(os.path.dirname(__file__), "heroes.json")
    try:
        with open(hero_file) as f:
            _HEROES = json.load(f)
        _HEROES_LOADED = True
    except FileNotFoundError:
        _HEROES = {}
        _HEROES_LOADED = True


def get_hero_name(hero_id: int) -> str:
    """
    Get hero name from hero ID.

    Args:
        hero_id: The Dota 2 hero ID

    Returns:
        Hero name or "Unknown Hero" if not found
    """
    _load_heroes()
    return _HEROES.get(str(hero_id), f"Hero {hero_id}")


def get_hero_short_name(hero_id: int) -> str:
    """
    Get a shortened hero name for compact display.

    Args:
        hero_id: The Dota 2 hero ID

    Returns:
        Shortened hero name (e.g., "AM" for Anti-Mage, "PA" for Phantom Assassin)
    """
    # Common abbreviations
    ABBREVIATIONS = {
        1: "AM",
        5: "CM",
        6: "Drow",
        7: "ES",
        11: "SF",
        12: "PL",
        17: "Storm",
        21: "WR",
        27: "SS",
        36: "Necro",
        39: "QoP",
        41: "Void",
        42: "WK",
        43: "DP",
        44: "PA",
        46: "TA",
        49: "DK",
        51: "Clock",
        53: "NP",
        54: "LS",
        55: "DS",
        60: "NS",
        62: "BH",
        68: "AA",
        71: "SB",
        72: "Gyro",
        74: "Invo",
        76: "OD",
        80: "LD",
        81: "CK",
        83: "Treant",
        84: "Ogre",
        88: "Nyx",
        90: "KotL",
        96: "Centaur",
        98: "Timber",
        99: "BB",
        101: "Sky",
        103: "ET",
        104: "LC",
        106: "Ember",
        107: "Earth",
        109: "TB",
        112: "WW",
        113: "AW",
        114: "MK",
        119: "Willow",
        120: "Pango",
        126: "Void Spirit",
        135: "Dawn",
        137: "PB",
    }

    if hero_id in ABBREVIATIONS:
        return ABBREVIATIONS[hero_id]

    # Fallback to first word of hero name
    name = get_hero_name(hero_id)
    if " " in name:
        return name.split()[0]
    return name


def get_all_heroes() -> dict[str, str]:
    """Get all heroes as a dict mapping hero_id (str) to name."""
    _load_heroes()
    return _HEROES.copy()
