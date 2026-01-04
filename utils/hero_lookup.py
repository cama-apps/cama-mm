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


# Steam CDN base URL for hero images
_STEAM_CDN_BASE = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes"

# Cache for hero info from dotabase
_HERO_INFO_CACHE: dict[int, dict] = {}


def _load_hero_info_from_dotabase():
    """Load hero info (slug, color) from dotabase if available."""
    global _HERO_INFO_CACHE
    if _HERO_INFO_CACHE:
        return

    try:
        from dotabase import Hero, dotabase_session

        session = dotabase_session()
        heroes = session.query(Hero).all()
        for hero in heroes:
            # Get the CDN slug (e.g., "antimage" from "npc_dota_hero_antimage")
            slug = hero.name if hero.name else ""
            if slug.startswith("npc_dota_hero_"):
                slug = slug[14:]  # Remove prefix

            _HERO_INFO_CACHE[hero.id] = {
                "slug": slug,
                "color": hero.color,  # e.g., "#784094"
                "localized_name": hero.localized_name,
            }
    except ImportError:
        pass
    except Exception:
        pass


def get_hero_image_url(hero_id: int, size: str = "full") -> str | None:
    """
    Get the Steam CDN URL for a hero image.

    Args:
        hero_id: The Dota 2 hero ID
        size: "full" for large image, "icon" for small icon

    Returns:
        URL to hero image, or None if hero not found
    """
    _load_hero_info_from_dotabase()

    if hero_id not in _HERO_INFO_CACHE:
        return None

    slug = _HERO_INFO_CACHE[hero_id]["slug"]
    if not slug:
        return None

    if size == "icon":
        return f"{_STEAM_CDN_BASE}/icons/{slug}.png"
    else:
        return f"{_STEAM_CDN_BASE}/{slug}.png"


def get_hero_color(hero_id: int) -> int | None:
    """
    Get the hero's color as an integer for Discord embed.

    Args:
        hero_id: The Dota 2 hero ID

    Returns:
        Color as integer (e.g., 0x784094), or None if not found
    """
    _load_hero_info_from_dotabase()

    if hero_id not in _HERO_INFO_CACHE:
        return None

    color_str = _HERO_INFO_CACHE[hero_id].get("color")
    if not color_str:
        return None

    try:
        # Convert "#784094" to 0x784094
        return int(color_str.lstrip("#"), 16)
    except (ValueError, TypeError):
        return None
