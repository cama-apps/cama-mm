from __future__ import annotations

import math
import sys
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, ImageOps

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

import postprocess

from utils import pet_compositor

WORK_DIR = Path(__file__).resolve().parent
QA_DIR = WORK_DIR / "qa"
PETS_DIR = postprocess.PETS_DIR
FONT = ImageFont.load_default()


def checkerboard(size: tuple[int, int], cell: int = 12) -> Image.Image:
    image = Image.new("RGBA", size, (215, 215, 215, 255))
    draw = ImageDraw.Draw(image)
    for y in range(0, size[1], cell):
        for x in range(0, size[0], cell):
            if (x // cell + y // cell) % 2:
                draw.rectangle(
                    (x, y, min(x + cell - 1, size[0]), min(y + cell - 1, size[1])),
                    fill=(170, 170, 170, 255),
                )
    return image


def label_tile(image: Image.Image, label: str, width: int) -> Image.Image:
    tile = Image.new("RGBA", (width, image.height + 22), (22, 22, 28, 255))
    tile.alpha_composite(image, ((width - image.width) // 2, 0))
    draw = ImageDraw.Draw(tile)
    draw.text((6, image.height + 5), label, fill=(245, 245, 245, 255), font=FONT)
    return tile


def sheet(tiles: list[Image.Image], columns: int, gap: int = 6) -> Image.Image:
    rows = math.ceil(len(tiles) / columns)
    tile_width = max(tile.width for tile in tiles)
    tile_height = max(tile.height for tile in tiles)
    result = Image.new(
        "RGBA",
        (
            columns * tile_width + (columns - 1) * gap,
            rows * tile_height + (rows - 1) * gap,
        ),
        (12, 12, 16, 255),
    )
    for index, tile in enumerate(tiles):
        x = (index % columns) * (tile_width + gap)
        y = (index // columns) * (tile_height + gap)
        result.alpha_composite(tile, (x, y))
    return result


def component_path(relative_path: str) -> Path:
    return PETS_DIR / relative_path


def create_registered_component_sheet() -> None:
    tiles: list[Image.Image] = []
    for raw_name, (relative_path, target_box, _alignment) in postprocess.COMPONENTS.items():
        with Image.open(component_path(relative_path)) as opened:
            component = opened.convert("RGBA")
        canvas = checkerboard(postprocess.CANVAS)
        canvas.alpha_composite(component)
        draw = ImageDraw.Draw(canvas)
        draw.rectangle(target_box, outline=(255, 70, 70, 255), width=2)
        preview = canvas.resize((256, 144), Image.Resampling.LANCZOS)
        tiles.append(label_tile(preview, raw_name, 256))
    sheet(tiles, columns=4).save(QA_DIR / "registered-components.png")


def create_cutout_sheet() -> None:
    tiles: list[Image.Image] = []
    for raw_name, (relative_path, _target_box, _alignment) in postprocess.COMPONENTS.items():
        with Image.open(component_path(relative_path)) as opened:
            component = opened.convert("RGBA")
        bbox = component.getchannel("A").getbbox()
        assert bbox is not None
        cutout = component.crop(bbox)
        fitted = ImageOps.contain(cutout, (236, 140), Image.Resampling.LANCZOS)
        canvas = checkerboard((256, 150), cell=10)
        canvas.alpha_composite(
            fitted,
            ((canvas.width - fitted.width) // 2, (canvas.height - fitted.height) // 2),
        )
        tiles.append(label_tile(canvas, raw_name, 256))
    sheet(tiles, columns=4).save(QA_DIR / "component-cutouts.png")


def create_scene_sheet() -> None:
    relative_paths = [
        "components/adult/backdrop/any_01.png",
        "components/adult/backdrop/any_02.png",
        "components/adult/backdrop/any_03.png",
        "egg.png",
        "tombstone.png",
    ]
    tiles: list[Image.Image] = []
    for relative_path in relative_paths:
        with Image.open(PETS_DIR / relative_path) as opened:
            image = opened.convert("RGBA")
        tiles.append(label_tile(image, relative_path, 512))
    sheet(tiles, columns=2).save(QA_DIR / "scenes.png")


def composed_image(species: str, stage: str, mood: str, seed: int) -> Image.Image:
    result = pet_compositor.compose_pet_card(species, stage, mood, seed)
    return Image.open(BytesIO(result.getvalue())).convert("RGBA")


def create_composite_sheets() -> None:
    pet_compositor.COMPONENTS_DIR = PETS_DIR / "components"
    pet_compositor.clear_caches()

    species = [
        "common_cama",
        "dromedary_cross",
        "banana_ears",
        "jopacama",
        "pudge_cama",
        "courier_cama",
        "aegis_cama",
        "invoker_cama",
        "crystal_cama",
        "rama",
    ]
    moods = ["happy", "neutral", "hungry"]
    adult_tiles = [
        label_tile(
            composed_image(species_id, "adult", moods[index % 3], 4100 + index),
            f"{species_id} / {moods[index % 3]}",
            512,
        )
        for index, species_id in enumerate(species)
    ]
    sheet(adult_tiles, columns=2).save(QA_DIR / "adult-composites.png")

    baby_tiles = [
        label_tile(
            composed_image("common_cama", "baby", moods[index % 3], 5100 + index),
            f"baby seed {5100 + index} / {moods[index % 3]}",
            512,
        )
        for index in range(9)
    ]
    sheet(baby_tiles, columns=2).save(QA_DIR / "baby-composites.png")


def validate() -> None:
    failures: list[str] = []
    pngs = sorted(PETS_DIR.rglob("*.png"))
    if len(pngs) != 29:
        failures.append(f"expected 29 delivered PNGs, found {len(pngs)}")

    component_root = PETS_DIR / "components"
    for path in pngs:
        with Image.open(path) as opened:
            image = opened.convert("RGBA")
            if opened.size != postprocess.CANVAS:
                failures.append(f"{path}: size {opened.size}")
            if opened.mode != "RGBA":
                failures.append(f"{path}: mode {opened.mode}")

        is_component = component_root in path.parents and "backdrop" not in path.parts
        if is_component:
            alpha = image.getchannel("A")
            if alpha.getextrema()[0] != 0 or alpha.getextrema()[1] == 0:
                failures.append(f"{path}: component alpha range {alpha.getextrema()}")
            corners = [
                alpha.getpixel((0, 0)),
                alpha.getpixel((511, 0)),
                alpha.getpixel((0, 287)),
                alpha.getpixel((511, 287)),
            ]
            if any(corners):
                failures.append(f"{path}: nontransparent corner {corners}")
        else:
            if image.getchannel("A").getextrema() != (255, 255):
                failures.append(f"{path}: scene is not fully opaque")

        if "creature" in path.parts:
            visible = [
                pixel
                for pixel in image.getdata()
                if pixel[3] > 16
            ]
            if any(red != green or green != blue for red, green, blue, _alpha in visible):
                failures.append(f"{path}: visible color contamination")
            values = [red for red, _green, _blue, _alpha in visible]
            if min(values) > 52 or max(values) < 236:
                failures.append(
                    f"{path}: grayscale range {min(values)}-{max(values)}"
                )

    for raw_name, (relative_path, target_box, _alignment) in postprocess.COMPONENTS.items():
        with Image.open(component_path(relative_path)) as opened:
            bbox = opened.convert("RGBA").getchannel("A").getbbox()
        assert bbox is not None
        left, top, right, bottom = target_box
        if bbox[0] < left or bbox[1] < top or bbox[2] > right or bbox[3] > bottom:
            failures.append(f"{raw_name}: bbox {bbox} outside target {target_box}")

    adult_backdrops = PETS_DIR / "components" / "adult" / "backdrop"
    baby_backdrops = PETS_DIR / "components" / "baby" / "backdrop"
    for name in ("any_01.png", "any_02.png", "any_03.png"):
        if (adult_backdrops / name).read_bytes() != (baby_backdrops / name).read_bytes():
            failures.append(f"baby backdrop copy differs: {name}")

    if failures:
        raise SystemExit("\n".join(failures))
    print(f"PASS: {len(pngs)} PNGs; exact RGBA/size/alpha/grayscale/zone checks")


def main() -> None:
    QA_DIR.mkdir(parents=True, exist_ok=True)
    validate()
    create_registered_component_sheet()
    create_cutout_sheet()
    create_scene_sheet()
    create_composite_sheets()


if __name__ == "__main__":
    main()
