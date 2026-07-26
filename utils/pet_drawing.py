"""
Procedural fallback sprite renderer for cama pet cards.

Generates 512x288 pixel-art pet portraits with PIL in the coarse-cell
style of utils/dig_drawing.py, tuned for collectible-creature charm:
big highlight eyes, blush marks, and one distinctive silhouette feature
per species. Every renderer is a pure function of its arguments —
randomness comes only from random.Random(seed), never the global random
module, so output bytes are deterministic.
"""

import io
import random
from dataclasses import dataclass

from PIL import Image, ImageDraw

from utils.fonts import get_font

CARD_WIDTH = 512
CARD_HEIGHT = 288
FLOOR_Y = 248

_BACKGROUND = (26, 22, 38)
_FLOOR = (46, 39, 60)
_FLOOR_LINE = (90, 78, 112)
_OUTLINE = (20, 15, 26)
_EYE_WHITE = (250, 250, 252)
_BLUSH = (242, 148, 158)
_TEAR = (150, 200, 240)

# Species palettes: (dark shade, body mid, light wool, accent).
SPECIES_PALETTES: dict[str, tuple[tuple[int, int, int], ...]] = {
    "common_cama":     ((122, 89, 53),  (181, 141, 96),  (215, 185, 141), (150, 110, 70)),
    "dromedary_cross": ((146, 106, 48), (206, 166, 98),  (234, 206, 152), (182, 142, 82)),
    "banana_ears":     ((86, 112, 60),  (142, 172, 98),  (198, 216, 152), (172, 192, 92)),
    "jopacama":        ((152, 108, 20), (218, 172, 42),  (250, 216, 112), (255, 196, 40)),
    "pudge_cama":      ((108, 48, 38),  (162, 82, 62),   (204, 130, 104), (140, 60, 50)),
    "courier_cama":    ((94, 56, 26),   (139, 90, 43),   (188, 138, 88),  (120, 76, 36)),
    "aegis_cama":      ((186, 172, 96), (234, 224, 150), (255, 250, 206), (255, 236, 140)),
    "invoker_cama":    ((94, 50, 128),  (138, 84, 182),  (186, 140, 222), (122, 70, 162)),
    "crystal_cama":    ((72, 118, 158), (128, 182, 220), (190, 226, 248), (162, 206, 236)),
    "rama":            ((150, 100, 96), (222, 214, 205), (248, 244, 238), (192, 52, 58)),
}
_DEFAULT_PALETTE = SPECIES_PALETTES["common_cama"]


@dataclass(frozen=True)
class _Geo:
    """Creature layout: a mirrored cell grid anchored to the floor line."""

    cell: int
    body_cols: int
    body_rows: int
    leg_rows: int
    neck_rows: int
    head_cols: int
    head_rows: int
    x0: int        # body left edge (px)
    body_w: int    # body width (px)
    body_top: int  # body top edge (px)
    hx: int        # head left edge (px)
    hy: int        # head top edge (px)
    hw: int        # head width (px)
    hh: int        # head height (px)


def _geometry(stage: str) -> _Geo:
    if stage == "baby":
        # Chibi proportions: near-adult head on a small body, no neck.
        cell, body_cols, body_rows, leg_rows, neck_rows, head_cols, head_rows = (
            12, 8, 4, 2, 0, 5, 4,
        )
    else:
        cell, body_cols, body_rows, leg_rows, neck_rows, head_cols, head_rows = (
            16, 10, 5, 3, 2, 4, 3,
        )
    body_w = body_cols * cell
    x0 = (CARD_WIDTH - body_w) // 2
    body_top = FLOOR_Y - (leg_rows + body_rows) * cell
    hw, hh = head_cols * cell, head_rows * cell
    return _Geo(
        cell, body_cols, body_rows, leg_rows, neck_rows, head_cols, head_rows,
        x0, body_w, body_top,
        (CARD_WIDTH - hw) // 2, body_top - (neck_rows + head_rows) * cell, hw, hh,
    )


def _to_png(img: Image.Image) -> io.BytesIO:
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    buf.seek(0)
    return buf


def _vignette(img: Image.Image) -> Image.Image:
    """Soft dark frame so the card reads like a portrait."""
    overlay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)
    w, h = img.size
    for ring in range(6):
        alpha = 36 - ring * 6
        inset = ring * 3
        draw.rectangle(
            [inset, inset, w - 1 - inset, h - 1 - inset],
            outline=(0, 0, 0, alpha),
            width=3,
        )
    return Image.alpha_composite(img, overlay)


def _card_base(rng: random.Random) -> Image.Image:
    """Dark stable backdrop: floor line plus seed-dependent dust motes."""
    img = Image.new("RGBA", (CARD_WIDTH, CARD_HEIGHT), (*_BACKGROUND, 255))
    draw = ImageDraw.Draw(img)
    draw.rectangle([0, FLOOR_Y, CARD_WIDTH, CARD_HEIGHT], fill=_FLOOR)
    draw.line([0, FLOOR_Y, CARD_WIDTH, FLOOR_Y], fill=_FLOOR_LINE, width=2)
    for _ in range(26):
        x = rng.randint(4, CARD_WIDTH - 5)
        y = rng.randint(4, FLOOR_Y - 20)
        draw.point((x, y), fill=(118, 110, 146))
    return img


def _half_blob(
    rng: random.Random, cols: int, rows: int, fullness: float
) -> list[tuple[int, int]]:
    """Left-half cells of a jittered ellipse; the caller mirrors them right."""
    cells: list[tuple[int, int]] = []
    cx, cy = (cols - 1) / 2, (rows - 1) / 2
    for row in range(rows):
        for col in range(cols // 2):
            dx = (col - cx) / (cols / 2)
            dy = (row - cy) / (rows / 2)
            if dx * dx + dy * dy <= fullness + rng.uniform(0.0, 0.45):
                cells.append((col, row))
    return cells


def _draw_back_features(draw: ImageDraw.ImageDraw, species_id: str, g: _Geo) -> None:
    """Silhouette features drawn behind the body: shell dome, hump."""
    cell, cx = g.cell, CARD_WIDTH // 2
    if species_id == "aegis_cama":
        shell = [
            g.x0 + cell // 2, g.body_top - int(1.9 * cell),
            g.x0 + g.body_w - cell // 2, g.body_top + int(2.5 * cell),
        ]
        draw.pieslice(shell, 180, 360, fill=(252, 206, 74))
        inner = [shell[0] + cell // 2, shell[1] + cell // 2,
                 shell[2] - cell // 2, shell[3] + cell // 2]
        draw.pieslice(inner, 180, 360, fill=(255, 240, 160))
    elif species_id == "dromedary_cross":
        hump = [
            cx - int(3.4 * cell), g.body_top - int(1.7 * cell),
            cx + int(3.4 * cell), g.body_top + int(1.5 * cell),
        ]
        draw.ellipse(hump, fill=(206, 166, 98))
        draw.ellipse(
            [hump[0] + cell // 2, hump[1] + 3, hump[2] - cell // 2, hump[1] + cell],
            fill=(234, 206, 152),
        )


def _draw_body_features(
    draw: ImageDraw.ImageDraw,
    rng: random.Random,
    species_id: str,
    g: _Geo,
    palette: tuple[tuple[int, int, int], ...],
) -> None:
    """Distinct per-species detail drawn on top of the body wool."""
    dark, _mid, light, accent = palette
    cell, x0, body_top = g.cell, g.x0, g.body_top
    if species_id in ("jopacama", "crystal_cama"):
        # Mirrored speckles: gold coins or ice glints in the wool.
        color = accent if species_id == "jopacama" else light
        radius = cell // 4 if species_id == "jopacama" else cell // 5
        for _ in range(5):
            col = rng.randint(1, g.body_cols // 2 - 1)
            row = rng.randint(1, g.body_rows - 2)
            for c in (col, g.body_cols - 1 - col):
                px = x0 + c * cell + cell // 2
                py = body_top + row * cell + cell // 2
                draw.ellipse([px - radius, py - radius, px + radius, py + radius], fill=color)
    if species_id == "crystal_cama":
        # Icicle fringe hanging from the belly.
        belly_y = body_top + g.body_rows * cell
        for apex_x in (
            x0 + 2 * cell + cell // 2,
            x0 + g.body_w // 2,
            x0 + g.body_w - 2 * cell - cell // 2,
        ):
            draw.polygon(
                [(apex_x - cell // 3, belly_y), (apex_x + cell // 3, belly_y),
                 (apex_x, belly_y + int(0.9 * cell))],
                fill=light,
            )
    elif species_id == "courier_cama":
        # Saddlebags on both flanks, with a lighter flap.
        for bx0, bx1 in (
            (x0 + int(0.4 * cell), x0 + int(2.2 * cell)),
            (x0 + g.body_w - int(2.2 * cell), x0 + g.body_w - int(0.4 * cell)),
        ):
            by0, by1 = body_top + int(1.4 * cell), body_top + int(3.6 * cell)
            draw.rectangle([bx0, by0, bx1, by1], fill=accent, outline=_OUTLINE)
            draw.rectangle([bx0, by0, bx1, by0 + (by1 - by0) // 3], fill=light)
    elif species_id == "rama":
        # Small red royal blanket with gold trim.
        by0, by1 = body_top + cell, body_top + 3 * cell
        draw.rectangle([x0 + 2 * cell, by0, x0 + g.body_w - 2 * cell, by1], fill=accent)
        draw.line([x0 + 2 * cell, by1, x0 + g.body_w - 2 * cell, by1],
                  fill=(255, 210, 90), width=2)
    elif species_id == "pudge_cama":
        # A tiny gray hook glinting from the wool.
        kx0 = x0 + g.body_w - int(2.4 * cell)
        ky0 = body_top + int(1.5 * cell)
        draw.arc([kx0, ky0, kx0 + cell, ky0 + cell], 0, 180, fill=(196, 198, 206), width=3)
        draw.line([kx0 + cell - 1, ky0 + cell // 2, kx0 + cell - 1, ky0 - cell // 2],
                  fill=(196, 198, 206), width=3)


def _draw_ears(
    draw: ImageDraw.ImageDraw,
    species_id: str,
    g: _Geo,
    mood: str,
    palette: tuple[tuple[int, int, int], ...],
) -> None:
    _dark, mid, _light, accent = palette
    cell = g.cell
    droop = cell // 2 if mood == "hungry" else 0
    if species_id == "banana_ears":
        w, h = int(1.1 * cell), int(2.6 * cell)  # oversized banana ears
    else:
        w, h = int(0.8 * cell), int(1.4 * cell)
    for side, rel in ((-1, 0.18), (1, 0.82)):
        ecx = g.hx + int(g.hw * rel)
        if species_id == "banana_ears":
            ecx += side * cell // 2  # tilt outward
        top = g.hy - h + cell // 2 + droop
        draw.ellipse([ecx - w // 2, top, ecx + w // 2, top + h], fill=mid)
        draw.ellipse(
            [ecx - w // 4, top + h // 4, ecx + w // 4, top + h - h // 4], fill=accent
        )


def _draw_face(
    draw: ImageDraw.ImageDraw,
    species_id: str,
    g: _Geo,
    mood: str,
    palette: tuple[tuple[int, int, int], ...],
) -> None:
    """Big highlight eyes, muzzle, and a mood-keyed mouth carry the cuteness."""
    _dark, mid, light, _accent = palette
    hx, hy, hw, hh = g.hx, g.hy, g.hw, g.hh
    er = max(4, hh // 5)
    ey = hy + int(hh * 0.42)
    mx = hx + hw // 2

    # Muzzle with nostrils.
    mw = int(hw * 0.24)
    draw.ellipse([mx - mw, hy + int(hh * 0.58), mx + mw, hy + hh - 3], fill=light)
    ny = hy + int(hh * 0.78) - er // 2 - 2
    for nx in (mx - er // 2, mx + er // 2):
        draw.rectangle([nx - 1, ny - 1, nx + 1, ny + 1], fill=_OUTLINE)

    for side, ecx in ((-1, hx + int(hw * 0.30)), (1, hx + int(hw * 0.70))):
        draw.ellipse([ecx - er, ey - er, ecx + er, ey + er], fill=_OUTLINE)
        hr = max(1, er // 3)
        draw.ellipse(
            [ecx - er // 3 - hr, ey - er // 2 - hr, ecx - er // 3 + hr, ey - er // 2 + hr],
            fill=_EYE_WHITE,
        )
        if mood == "hungry":
            draw.rectangle([ecx - er, ey - er, ecx + er, ey - er // 4], fill=mid)
            if side < 0:  # a single tear under the left eye
                draw.ellipse(
                    [ecx - er // 4, ey + er, ecx + er // 4, ey + 2 * er], fill=_TEAR
                )
        if species_id == "rama":  # grumpy brows: Rama never looks fully happy
            draw.line(
                [ecx + side * er, ey - 2 * er, ecx - side * er, ey - int(1.3 * er)],
                fill=_OUTLINE, width=3,
            )

    my = hy + int(hh * 0.78)
    if mood == "happy":
        draw.arc([mx - er, my - er // 2, mx + er, my + er // 2], 20, 160,
                 fill=_OUTLINE, width=3)
        if species_id != "rama":
            for bx in (hx + int(hw * 0.13), hx + int(hw * 0.87)):
                draw.ellipse(
                    [bx - er // 2, ey + er // 2, bx + er // 2, ey + er], fill=_BLUSH
                )
    elif mood == "hungry":
        draw.arc([mx - er, my, mx + er, my + er], 200, 340, fill=_OUTLINE, width=3)
    else:
        draw.line([mx - er, my, mx + er, my], fill=_OUTLINE, width=3)


def _draw_creature(
    draw: ImageDraw.ImageDraw,
    rng: random.Random,
    species_id: str,
    g: _Geo,
    mood: str,
) -> None:
    palette = SPECIES_PALETTES.get(species_id, _DEFAULT_PALETTE)
    dark, mid, light, _accent = palette
    cell = g.cell

    draw.ellipse(
        [g.x0 - cell, FLOOR_Y - cell // 2, g.x0 + g.body_w + cell, FLOOR_Y + cell // 2],
        fill=(30, 25, 44),
    )
    _draw_back_features(draw, species_id, g)

    # Legs (mirrored pairs) with light hooves.
    leg_top = FLOOR_Y - g.leg_rows * cell
    for col in (1, 3):
        for c in (col, g.body_cols - 1 - col):
            lx = g.x0 + c * cell
            draw.rectangle([lx, leg_top, lx + cell - 1, FLOOR_Y - 1], fill=dark)
            draw.rectangle([lx, FLOOR_Y - cell // 3, lx + cell - 1, FLOOR_Y - 1], fill=light)

    # Body: jittered ellipse built on the left half, mirrored onto the right.
    # Babies get a fuller blob so the small grid still reads as a round body.
    fullness = 0.78 if species_id == "pudge_cama" else 0.70 if g.neck_rows == 0 else 0.62
    for col, row in _half_blob(rng, g.body_cols, g.body_rows, fullness):
        color = (
            light if row < g.body_rows // 3
            else mid if row < 2 * g.body_rows // 3
            else dark
        )
        for c in (col, g.body_cols - 1 - col):
            x = g.x0 + c * cell
            y = g.body_top + row * cell
            draw.rectangle([x, y, x + cell - 1, y + cell - 1], fill=color)

    _draw_body_features(draw, rng, species_id, g, palette)

    if g.neck_rows:
        cx = CARD_WIDTH // 2
        draw.rectangle(
            [cx - cell, g.hy + g.hh - cell, cx + cell, g.body_top], fill=mid
        )

    _draw_ears(draw, species_id, g, mood, palette)

    # Round head with a lighter forehead.
    draw.ellipse([g.hx, g.hy, g.hx + g.hw, g.hy + g.hh], fill=mid)
    draw.ellipse(
        [g.hx + int(g.hw * 0.15), g.hy + 3, g.hx + int(g.hw * 0.85), g.hy + int(g.hh * 0.45)],
        fill=light,
    )
    _draw_face(draw, species_id, g, mood, palette)

    if species_id == "invoker_cama":  # Quas, Wex, Exort orbiting the head
        orb_r = max(3, cell // 3)
        for (ox, oy), color in (
            ((g.hx + int(g.hw * 0.08), g.hy - int(1.1 * cell)), (130, 205, 255)),
            ((g.hx + g.hw // 2, g.hy - int(1.7 * cell)), (205, 150, 255)),
            ((g.hx + int(g.hw * 0.92), g.hy - int(1.1 * cell)), (255, 175, 80)),
        ):
            draw.ellipse([ox - orb_r, oy - orb_r, ox + orb_r, oy + orb_r], fill=color)
            draw.point((ox - orb_r // 2, oy - orb_r // 2), fill=_EYE_WHITE)


def render_pet_card(species_id: str, stage: str, mood: str, seed: int) -> io.BytesIO:
    """Render a deterministic 512x288 pet portrait PNG."""
    rng = random.Random(seed)
    img = _card_base(rng)
    draw = ImageDraw.Draw(img)
    _draw_creature(draw, rng, species_id, _geometry(stage), mood)
    return _to_png(_vignette(img))


def render_egg_card(seed: int) -> io.BytesIO:
    """Render a warm-lit speckled egg resting on a straw nest."""
    rng = random.Random(seed)
    img = _card_base(rng)
    cx = CARD_WIDTH // 2

    glow = Image.new("RGBA", (CARD_WIDTH, CARD_HEIGHT), (0, 0, 0, 0))
    glow_draw = ImageDraw.Draw(glow)
    for r in range(140, 20, -12):
        alpha = int(70 * (1 - r / 150))
        glow_draw.ellipse([cx - r, 150 - r, cx + r, 150 + r], fill=(255, 190, 90, alpha))
    img = Image.alpha_composite(img, glow)
    draw = ImageDraw.Draw(img)

    draw.ellipse([cx - 130, FLOOR_Y - 40, cx + 130, FLOOR_Y + 18], fill=(96, 70, 34))
    for _ in range(40):
        sx = rng.randint(cx - 120, cx + 120)
        sy = rng.randint(FLOOR_Y - 34, FLOOR_Y + 8)
        draw.line(
            [sx, sy, sx + rng.randint(-18, 18), sy + rng.randint(-4, 4)],
            fill=(178, 140, 70), width=2,
        )

    rx, ry = 58, 76
    ecy = FLOOR_Y - 30 - ry
    draw.ellipse(
        [cx - rx, ecy - ry, cx + rx, ecy + ry],
        fill=(240, 230, 204), outline=(120, 100, 70), width=3,
    )
    for _ in range(16):
        sx = rng.randint(cx - rx + 10, cx + rx - 10)
        sy = rng.randint(ecy - ry + 12, ecy + ry - 12)
        if ((sx - cx) / rx) ** 2 + ((sy - ecy) / ry) ** 2 <= 0.6:
            draw.ellipse([sx, sy, sx + 6, sy + 6], fill=(176, 150, 118))
    draw.ellipse([cx - 40, ecy - ry + 16, cx - 16, ecy - ry + 42], fill=(250, 246, 232))
    for _ in range(10):  # straw strands tucked over the egg's base
        sx = rng.randint(cx - rx, cx + rx)
        sy = rng.randint(FLOOR_Y - 44, FLOOR_Y - 26)
        draw.line([sx, sy, sx + rng.randint(-16, 16), sy + 3], fill=(190, 152, 78), width=2)

    return _to_png(_vignette(img))


def render_tombstone_card(name: str) -> io.BytesIO:
    """Render a soft twilight tombstone engraved with the pet's name."""
    rng = random.Random(0)
    img = Image.new("RGBA", (CARD_WIDTH, CARD_HEIGHT), (18, 16, 40, 255))
    draw = ImageDraw.Draw(img)
    for _ in range(40):
        draw.point(
            (rng.randint(4, CARD_WIDTH - 5), rng.randint(4, 150)),
            fill=(210, 214, 240),
        )
    glow = Image.new("RGBA", (CARD_WIDTH, CARD_HEIGHT), (0, 0, 0, 0))
    glow_draw = ImageDraw.Draw(glow)
    for r in range(60, 12, -8):
        glow_draw.ellipse(
            [408 - r, 56 - r, 408 + r, 56 + r],
            fill=(232, 232, 250, int(50 * (1 - r / 64))),
        )
    glow_draw.ellipse([408 - 14, 56 - 14, 408 + 14, 56 + 14], fill=(238, 238, 252, 255))
    img = Image.alpha_composite(img, glow)
    draw = ImageDraw.Draw(img)

    draw.rectangle([0, FLOOR_Y, CARD_WIDTH, CARD_HEIGHT], fill=(30, 40, 34))
    draw.ellipse([-80, FLOOR_Y - 26, CARD_WIDTH + 80, FLOOR_Y + 60], fill=(36, 50, 40))

    stone_w, stone_h, arc_h = 220, 150, 64
    x0 = (CARD_WIDTH - stone_w) // 2
    y0 = FLOOR_Y - stone_h
    top = y0 - arc_h
    stone, edge = (118, 118, 126), (58, 58, 66)
    draw.pieslice([x0, top, x0 + stone_w, top + 2 * arc_h], 180, 360,
                  fill=stone, outline=edge, width=3)
    draw.rectangle([x0, top + arc_h, x0 + stone_w, FLOOR_Y], fill=stone,
                   outline=edge, width=3)
    draw.rectangle([x0 + 3, top + arc_h - 4, x0 + stone_w - 3, top + arc_h + 4], fill=stone)

    # Flowers at the base.
    for fx, color in (
        (x0 - 44, (240, 150, 160)), (x0 - 24, (200, 170, 240)),
        (x0 + stone_w + 24, (240, 200, 120)), (x0 + stone_w + 44, (170, 200, 240)),
    ):
        base_y = FLOOR_Y + 6
        draw.line([fx, base_y, fx, base_y - 14], fill=(70, 110, 70), width=2)
        for ddx, ddy in ((-4, -18), (4, -18), (0, -22), (0, -14)):
            draw.ellipse(
                [fx + ddx - 3, base_y + ddy - 3, fx + ddx + 3, base_y + ddy + 3],
                fill=color,
            )
        draw.ellipse([fx - 2, base_y - 20, fx + 2, base_y - 16], fill=(255, 230, 140))

    rip_font = get_font(30, bold=True)
    name_font = get_font(20)
    shown = name.strip() or "Unnamed"
    max_w = stone_w - 28
    if draw.textlength(shown, font=name_font) > max_w:
        while len(shown) > 1 and draw.textlength(shown + "...", font=name_font) > max_w:
            shown = shown[:-1]
        shown += "..."
    for text, font, ty in (
        ("R.I.P.", rip_font, top + arc_h - 10),
        (shown, name_font, y0 + 42),
    ):
        tw = draw.textlength(text, font=font)
        tx = (CARD_WIDTH - tw) // 2
        draw.text((tx + 1, ty + 1), text, font=font, fill=(158, 158, 166))
        draw.text((tx, ty), text, font=font, fill=(48, 48, 56))

    return _to_png(_vignette(img))
