"""
Tile-based pixel art scene generator for dig minigame events.

Generates 320x180 retro dungeon crawler scenes using PIL.
Each layer has a color palette and tile set. Events overlay
creatures, items, and effects on the dungeon background.
"""

import io
import random
from dataclasses import dataclass

from PIL import Image, ImageDraw

# ---------------------------------------------------------------------------
# Tile Size & Scene Dimensions
# ---------------------------------------------------------------------------

TILE_SIZE = 16
SCENE_WIDTH = 320
SCENE_HEIGHT = 180

# ---------------------------------------------------------------------------
# Layer Color Palettes (4 colors: darkest, dark, mid, light)
# ---------------------------------------------------------------------------

LAYER_PALETTES: dict[str, tuple[tuple[int, int, int], ...]] = {
    "Dirt":          ((59, 31, 10),  (139, 69, 19),  (160, 82, 45),  (210, 180, 140)),
    "Stone":         ((50, 50, 50),  (105, 105, 105),(128, 128, 128),(169, 169, 169)),
    "Crystal":       ((0, 80, 80),   (0, 139, 139),  (0, 206, 209),  (127, 255, 212)),
    "Magma":         ((80, 0, 0),    (139, 0, 0),    (255, 69, 0),   (255, 215, 0)),
    "Abyss":         ((10, 0, 25),   (26, 0, 51),    (47, 0, 71),    (75, 0, 130)),
    "Fungal Depths": ((0, 40, 0),    (0, 100, 0),    (34, 139, 34),  (124, 252, 0)),
    "Frozen Core":   ((0, 0, 60),    (0, 0, 128),    (65, 105, 225), (135, 206, 235)),
    "The Hollow":    ((3, 3, 3),     (10, 10, 10),   (20, 20, 20),   (40, 40, 40)),
}

# Player sprite color
PLAYER_COLOR = (255, 255, 100)  # Bright yellow @

# ---------------------------------------------------------------------------
# Sprite Drawing Helpers
# ---------------------------------------------------------------------------

# PIL Image cache — bounded by the finite set of layers/sprites (~30 entries, ~10 MB).
_cache: dict[str, Image.Image] = {}


def _draw_tile(palette: tuple, variant: str = "wall", rng: random.Random | None = None) -> Image.Image:
    """Draw a single 16x16 tile with the given palette.

    Cache key ignores *rng* — safe because each palette maps to exactly one
    layer, so the seeded RNG always produces the same result per palette.
    """
    key = f"tile_{palette}_{variant}"
    if key in _cache:
        return _cache[key]

    _rng = rng or random.Random()
    img = Image.new("RGB", (TILE_SIZE, TILE_SIZE), palette[0])
    draw = ImageDraw.Draw(img)

    if variant == "wall":
        # Brick-like wall pattern
        draw.rectangle([0, 0, TILE_SIZE - 1, TILE_SIZE - 1], fill=palette[1])
        # Mortar lines
        draw.line([0, TILE_SIZE // 2, TILE_SIZE, TILE_SIZE // 2], fill=palette[0], width=1)
        draw.line([TILE_SIZE // 2, 0, TILE_SIZE // 2, TILE_SIZE // 2], fill=palette[0], width=1)
        draw.line([0, 0, 0, TILE_SIZE], fill=palette[0], width=1)
        # Highlight
        draw.point([(3, 3), (11, 11)], fill=palette[2])

    elif variant == "floor":
        # Subtle stone floor
        draw.rectangle([0, 0, TILE_SIZE - 1, TILE_SIZE - 1], fill=palette[0])
        # Speckle
        for _ in range(4):
            x, y = _rng.randint(1, 14), _rng.randint(1, 14)
            draw.point((x, y), fill=palette[1])

    elif variant == "torch":
        # Torch on wall
        draw.rectangle([0, 0, TILE_SIZE - 1, TILE_SIZE - 1], fill=palette[1])
        # Torch body
        draw.rectangle([6, 6, 9, 14], fill=(100, 60, 20))
        # Flame
        draw.ellipse([5, 2, 10, 7], fill=(255, 200, 50))
        draw.ellipse([6, 3, 9, 6], fill=(255, 255, 150))

    _cache[key] = img
    return img


def _draw_player_sprite() -> Image.Image:
    """Draw a 16x16 player @ sprite."""
    if "player" in _cache:
        return _cache["player"]

    img = Image.new("RGBA", (TILE_SIZE, TILE_SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    # Simple @ character as pixel art
    # Head
    draw.ellipse([4, 1, 11, 8], fill=PLAYER_COLOR)
    # Body
    draw.rectangle([5, 8, 10, 14], fill=PLAYER_COLOR)
    # Eyes
    draw.point([(6, 4), (9, 4)], fill=(0, 0, 0))
    # Pickaxe
    draw.line([11, 6, 14, 3], fill=(180, 180, 180), width=1)
    draw.point((14, 2), fill=(200, 200, 200))

    _cache["player"] = img
    return img


def _draw_creature_sprite(creature_type: str) -> Image.Image:
    """Draw a 16x16 creature sprite."""
    key = f"creature_{creature_type}"
    if key in _cache:
        return _cache[key]

    img = Image.new("RGBA", (TILE_SIZE, TILE_SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    if creature_type == "boss":
        # Large menacing figure
        draw.rectangle([2, 2, 13, 14], fill=(200, 50, 50))
        draw.point([(5, 5), (10, 5)], fill=(255, 255, 0))  # Glowing eyes
        draw.rectangle([4, 9, 11, 12], fill=(150, 30, 30))  # Mouth
    elif creature_type == "pudge":
        # Rotund figure with hook
        draw.ellipse([2, 3, 13, 14], fill=(120, 80, 60))
        draw.point([(5, 6), (10, 6)], fill=(255, 0, 0))
        draw.line([13, 5, 15, 1], fill=(150, 150, 150), width=1)
    elif creature_type == "tinker":
        # Small figure with goggles
        draw.rectangle([4, 4, 11, 14], fill=(80, 80, 120))
        draw.ellipse([4, 2, 11, 8], fill=(100, 100, 140))
        draw.ellipse([5, 4, 7, 6], fill=(200, 200, 50))  # Goggle
        draw.ellipse([8, 4, 10, 6], fill=(200, 200, 50))
    elif creature_type == "roshan":
        # Massive figure
        draw.rectangle([1, 1, 14, 14], fill=(100, 50, 20))
        draw.point([(4, 4), (11, 4)], fill=(255, 100, 0))
        draw.rectangle([3, 8, 12, 12], fill=(80, 40, 15))
        # Horns
        draw.line([3, 2, 1, 0], fill=(180, 140, 80), width=1)
        draw.line([12, 2, 14, 0], fill=(180, 140, 80), width=1)
    elif creature_type == "merchant":
        draw.rectangle([4, 4, 11, 14], fill=(80, 60, 100))
        draw.ellipse([4, 1, 11, 7], fill=(100, 80, 120))
        draw.rectangle([6, 12, 9, 15], fill=(60, 40, 80))
    elif creature_type == "ghost":
        draw.ellipse([3, 2, 12, 10], fill=(180, 180, 220))
        draw.point([(6, 5), (9, 5)], fill=(100, 100, 150))
        # Wavy bottom
        for x in range(3, 13):
            y = 10 + (x % 2) * 2
            draw.point((x, y), fill=(180, 180, 220))
    else:
        # Generic creature
        draw.rectangle([4, 3, 11, 14], fill=(150, 100, 80))
        draw.point([(6, 5), (9, 5)], fill=(255, 255, 255))

    _cache[key] = img
    return img


def _draw_item_sprite(item_type: str) -> Image.Image:
    """Draw a 16x16 item sprite."""
    key = f"item_{item_type}"
    if key in _cache:
        return _cache[key]

    img = Image.new("RGBA", (TILE_SIZE, TILE_SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    if item_type == "chest":
        draw.rectangle([2, 6, 13, 14], fill=(139, 90, 43))
        draw.rectangle([2, 6, 13, 8], fill=(160, 110, 60))
        draw.rectangle([6, 9, 9, 11], fill=(255, 215, 0))  # Lock
    elif item_type == "treasure":
        draw.ellipse([3, 8, 12, 14], fill=(255, 215, 0))
        draw.ellipse([5, 5, 10, 10], fill=(255, 215, 0))
        draw.ellipse([7, 3, 9, 6], fill=(255, 235, 100))
    elif item_type == "mushroom":
        draw.ellipse([3, 2, 12, 8], fill=(200, 50, 50))
        draw.rectangle([6, 8, 9, 14], fill=(200, 180, 150))
        draw.point([(5, 4), (9, 4), (7, 3)], fill=(255, 255, 255))
    elif item_type == "crystal":
        draw.polygon([(7, 1), (12, 7), (7, 14), (3, 7)], fill=(100, 200, 255))
        draw.polygon([(7, 3), (10, 7), (7, 12), (5, 7)], fill=(150, 220, 255))
    elif item_type == "book":
        draw.rectangle([3, 3, 12, 13], fill=(100, 40, 40))
        draw.rectangle([4, 4, 11, 12], fill=(200, 180, 140))
        draw.line([7, 4, 7, 12], fill=(100, 40, 40), width=1)
    elif item_type == "water":
        draw.rectangle([0, 6, 15, 15], fill=(30, 60, 180))
        for x in range(0, 16, 3):
            draw.arc([x, 4, x + 4, 8], 0, 180, fill=(60, 100, 220))
    else:
        draw.rectangle([4, 4, 11, 11], fill=(200, 200, 200))

    _cache[key] = img
    return img


# ---------------------------------------------------------------------------
# Scene Element Definition
# ---------------------------------------------------------------------------

@dataclass
class SceneElement:
    """An element to place in a scene."""
    sprite_type: str          # "creature", "item", "effect"
    sprite_id: str            # e.g. "pudge", "chest", "fire"
    x: float                  # 0.0-1.0 relative x position
    y: float                  # 0.0-1.0 relative y position


# ---------------------------------------------------------------------------
# Event Scene Registry
# ---------------------------------------------------------------------------

EVENT_SCENES: dict[str, list[SceneElement]] = {
    "pudge_fishing": [
        SceneElement("item", "water", 0.3, 0.55),
        SceneElement("creature", "pudge", 0.75, 0.4),
    ],
    "tinker_workshop": [
        SceneElement("item", "chest", 0.5, 0.6),
        SceneElement("creature", "tinker", 0.7, 0.35),
        SceneElement("item", "crystal", 0.4, 0.3),
    ],
    "roshan_lair": [
        SceneElement("creature", "roshan", 0.5, 0.3),
        SceneElement("item", "treasure", 0.45, 0.65),
    ],
    "arcanist_library": [
        SceneElement("item", "book", 0.3, 0.2),
        SceneElement("item", "book", 0.5, 0.2),
        SceneElement("item", "book", 0.7, 0.2),
        SceneElement("creature", "ghost", 0.65, 0.4),
    ],
    "the_dark_rift": [
        SceneElement("creature", "boss", 0.6, 0.3),
    ],
    "the_burrow": [
        SceneElement("creature", "boss", 0.7, 0.5),
    ],
    "toll_keeper": [
        SceneElement("creature", "ghost", 0.6, 0.35),
        SceneElement("item", "treasure", 0.6, 0.65),
    ],
    "mirror_tunnel": [
        SceneElement("item", "crystal", 0.3, 0.3),
        SceneElement("item", "crystal", 0.7, 0.3),
        SceneElement("item", "crystal", 0.5, 0.5),
    ],
    "void_market": [
        SceneElement("creature", "merchant", 0.5, 0.35),
        SceneElement("creature", "merchant", 0.7, 0.35),
        SceneElement("item", "treasure", 0.6, 0.6),
    ],
    "time_eddy": [
        SceneElement("item", "crystal", 0.5, 0.4),
    ],
    "paradox_loop": [],  # Empty — player meets themselves
    "the_cartographer": [
        SceneElement("creature", "ghost", 0.65, 0.4),
        SceneElement("item", "book", 0.5, 0.6),
    ],
    "the_final_merchant": [
        SceneElement("creature", "merchant", 0.6, 0.35),
        SceneElement("item", "chest", 0.6, 0.6),
    ],
    "spore_storm": [
        SceneElement("item", "mushroom", 0.3, 0.5),
        SceneElement("item", "mushroom", 0.5, 0.4),
        SceneElement("item", "mushroom", 0.7, 0.55),
    ],
    "mycelium_network": [
        SceneElement("item", "mushroom", 0.4, 0.45),
        SceneElement("item", "mushroom", 0.6, 0.45),
    ],
    "bioluminescent_cathedral": [
        SceneElement("item", "crystal", 0.3, 0.15),
        SceneElement("item", "crystal", 0.5, 0.1),
        SceneElement("item", "crystal", 0.7, 0.15),
    ],
    "frozen_ancient": [
        SceneElement("creature", "boss", 0.6, 0.35),
        SceneElement("item", "crystal", 0.5, 0.5),
    ],
    "the_lightless_path": [],  # Darkness — no visible elements
    "whispering_walls_extended": [],
}


# ---------------------------------------------------------------------------
# Scene Composition
# ---------------------------------------------------------------------------

def _get_layer_background(layer_name: str, width: int, height: int) -> Image.Image:
    """Generate a tiled dungeon background for a layer."""
    key = f"bg_{layer_name}_{width}_{height}"
    if key in _cache:
        return _cache[key].copy()

    palette = LAYER_PALETTES.get(layer_name, LAYER_PALETTES["Dirt"])
    img = Image.new("RGB", (width, height), palette[0])

    # Fill with floor tiles (local RNG to avoid corrupting global state)
    _rng = random.Random(hash(layer_name))
    floor_tile = _draw_tile(palette, "floor", _rng)
    for tx in range(0, width, TILE_SIZE):
        for ty in range(0, height, TILE_SIZE):
            img.paste(floor_tile, (tx, ty))

    # Draw wall border (top 2 rows, bottom row, left/right columns)
    wall_tile = _draw_tile(palette, "wall")
    for tx in range(0, width, TILE_SIZE):
        img.paste(wall_tile, (tx, 0))
        img.paste(wall_tile, (tx, TILE_SIZE))
        img.paste(wall_tile, (tx, height - TILE_SIZE))
    for ty in range(0, height, TILE_SIZE):
        img.paste(wall_tile, (0, ty))
        img.paste(wall_tile, (TILE_SIZE, ty))
        img.paste(wall_tile, (width - TILE_SIZE, ty))
        img.paste(wall_tile, (width - 2 * TILE_SIZE, ty))

    # Add torches
    torch_tile = _draw_tile(palette, "torch")
    torch_positions = [(3 * TILE_SIZE, TILE_SIZE), (width - 4 * TILE_SIZE, TILE_SIZE)]
    for tx, ty in torch_positions:
        img.paste(torch_tile, (tx, ty))

    # Torch glow effect
    glow = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    glow_draw = ImageDraw.Draw(glow)
    for tx, ty in torch_positions:
        cx, cy = tx + TILE_SIZE // 2, ty + TILE_SIZE // 2
        for r in range(60, 10, -5):
            alpha = max(5, 30 - r // 2)
            glow_draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 180, 50, alpha))

    img = Image.alpha_composite(img.convert("RGBA"), glow).convert("RGB")

    _cache[key] = img
    return img.copy()


def _get_retro_overlay(width: int, height: int) -> Image.Image:
    """Get or create a cached CRT scanline overlay."""
    key = f"overlay_{width}_{height}"
    if key in _cache:
        return _cache[key]

    overlay = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)

    # Scanlines every 2 pixels
    for y in range(0, height, 2):
        draw.line([(0, y), (width, y)], fill=(0, 0, 0, 25))

    # Vignette at edges (line draws instead of per-pixel points)
    for border in range(3):
        alpha = 40 - border * 12
        if alpha > 0:
            draw.line([(0, border), (width - 1, border)], fill=(0, 0, 0, alpha))
            draw.line([(0, height - 1 - border), (width - 1, height - 1 - border)], fill=(0, 0, 0, alpha))

    _cache[key] = overlay
    return overlay


def _apply_retro_overlay(img: Image.Image) -> Image.Image:
    """Apply subtle CRT scanline overlay for retro feel."""
    overlay = _get_retro_overlay(img.size[0], img.size[1])
    result = Image.alpha_composite(img.convert("RGBA"), overlay)
    return result.convert("RGB")


def draw_event_scene(layer_name: str, event_id: str) -> io.BytesIO:
    """
    Generate a pixel art scene for a dig event.

    Returns a BytesIO buffer containing the PNG image.
    """
    width, height = SCENE_WIDTH, SCENE_HEIGHT

    # 1. Layer background with dungeon frame
    img = _get_layer_background(layer_name, width, height)

    # 2. Draw event-specific foreground elements
    scene_elements = EVENT_SCENES.get(event_id, [])
    for elem in scene_elements:
        px = int(elem.x * width)
        py = int(elem.y * height)

        if elem.sprite_type == "creature":
            sprite = _draw_creature_sprite(elem.sprite_id)
        elif elem.sprite_type == "item":
            sprite = _draw_item_sprite(elem.sprite_id)
        else:
            continue

        img.paste(sprite, (px, py), sprite)

    # 3. Draw player sprite
    player = _draw_player_sprite()
    player_x = width // 4
    player_y = height // 2
    img.paste(player, (player_x, player_y), player)

    # 4. Apply retro CRT overlay
    img = _apply_retro_overlay(img)

    # 5. Save to buffer
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return buf


def draw_boss_scene(layer_name: str, boss_name: str) -> io.BytesIO:
    """Generate a pixel art scene for a boss encounter."""
    width, height = SCENE_WIDTH, SCENE_HEIGHT

    img = _get_layer_background(layer_name, width, height)

    # Boss creature in center
    boss = _draw_creature_sprite("boss")
    # Draw boss larger (2x scale)
    boss_big = boss.resize((TILE_SIZE * 2, TILE_SIZE * 2), Image.Resampling.NEAREST)
    boss_x = width // 2 - TILE_SIZE
    boss_y = height // 3
    img.paste(boss_big, (boss_x, boss_y), boss_big)

    # Player sprite
    player = _draw_player_sprite()
    img.paste(player, (width // 4, height * 2 // 3), player)

    img = _apply_retro_overlay(img)

    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return buf


def draw_boss_result_scene(layer_name: str, boss_name: str, won: bool) -> io.BytesIO:
    """Generate a pixel art scene for a boss fight result (victory or defeat)."""
    width, height = SCENE_WIDTH, SCENE_HEIGHT
    img = _get_layer_background(layer_name, width, height)

    boss = _draw_creature_sprite("boss")
    player = _draw_player_sprite()

    if won:
        # Victory: faded boss, large player, gold sparkles
        boss_big = boss.resize((TILE_SIZE * 2, TILE_SIZE * 2), Image.Resampling.NEAREST)
        faded = boss_big.copy()
        faded.putalpha(faded.getchannel("A").point(lambda a: a // 2))
        boss_x = width // 2 - TILE_SIZE
        boss_y = height // 3
        img.paste(faded, (boss_x, boss_y), faded)

        # Player 2x, center-right
        player_big = player.resize((TILE_SIZE * 2, TILE_SIZE * 2), Image.Resampling.NEAREST)
        img.paste(player_big, (width // 3 - TILE_SIZE, height // 2 - TILE_SIZE), player_big)

        # Gold sparkle overlay
        sparkle = Image.new("RGBA", (width, height), (0, 0, 0, 0))
        sparkle_draw = ImageDraw.Draw(sparkle)
        rng = random.Random(42)
        for _ in range(20):
            sx, sy = rng.randint(40, width - 40), rng.randint(20, height - 20)
            sparkle_draw.ellipse([sx - 2, sy - 2, sx + 2, sy + 2], fill=(255, 215, 0, 160))
        img = Image.alpha_composite(img.convert("RGBA"), sparkle).convert("RGB")
    else:
        # Defeat: huge boss, small knocked-back player, red tint
        boss_huge = boss.resize((TILE_SIZE * 3, TILE_SIZE * 3), Image.Resampling.NEAREST)
        boss_x = width // 2 - (TILE_SIZE * 3) // 2
        boss_y = height // 4
        img.paste(boss_huge, (boss_x, boss_y), boss_huge)

        # Player small, bottom-left
        img.paste(player, (width // 6, height * 3 // 4), player)

        # Red tint overlay
        red_tint = Image.new("RGBA", (width, height), (180, 0, 0, 35))
        img = Image.alpha_composite(img.convert("RGBA"), red_tint).convert("RGB")

    img = _apply_retro_overlay(img)

    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return buf


def draw_layer_thumbnail(layer_name: str) -> io.BytesIO:
    """Generate a 128x128 pixel art thumbnail for a layer."""
    key = f"layer_thumb_{layer_name}"
    if key in _cache:
        buf = io.BytesIO()
        _cache[key].save(buf, format="PNG", optimize=True)
        buf.seek(0)
        return buf

    size = 128
    palette = LAYER_PALETTES.get(layer_name, LAYER_PALETTES["Dirt"])
    img = Image.new("RGB", (size, size), palette[0])

    rng = random.Random(hash(layer_name))
    floor_tile = _draw_tile(palette, "floor", rng)
    wall_tile = _draw_tile(palette, "wall")

    # Fill with floor tiles
    for tx in range(0, size, TILE_SIZE):
        for ty in range(0, size, TILE_SIZE):
            img.paste(floor_tile, (tx, ty))

    # Wall border (top, bottom, left, right)
    for tx in range(0, size, TILE_SIZE):
        img.paste(wall_tile, (tx, 0))
        img.paste(wall_tile, (tx, size - TILE_SIZE))
    for ty in range(0, size, TILE_SIZE):
        img.paste(wall_tile, (0, ty))
        img.paste(wall_tile, (size - TILE_SIZE, ty))

    # Torch in center-top
    torch_tile = _draw_tile(palette, "torch")
    img.paste(torch_tile, (size // 2 - TILE_SIZE // 2, TILE_SIZE))

    # Torch glow
    glow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    glow_draw = ImageDraw.Draw(glow)
    cx, cy = size // 2, TILE_SIZE + TILE_SIZE // 2
    for r in range(40, 10, -5):
        alpha = max(5, 25 - r // 2)
        glow_draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 180, 50, alpha))
    img = Image.alpha_composite(img.convert("RGBA"), glow).convert("RGB")

    img = _apply_retro_overlay(img)

    _cache[key] = img

    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return buf


def has_event_scene(event_id: str) -> bool:
    """Check if an event has a registered pixel art scene."""
    return event_id in EVENT_SCENES
