from __future__ import annotations

import shutil
from pathlib import Path

from PIL import Image, ImageOps

WORK_DIR = Path(__file__).resolve().parent
RAW_DIR = WORK_DIR / "raw"
KEYED_DIR = WORK_DIR / "keyed"
PACK_DIR = WORK_DIR.parent / "cama-pets-art-pack"
PETS_DIR = PACK_DIR / "assets" / "pets"

CANVAS = (512, 288)
LANCZOS = Image.Resampling.LANCZOS


# raw filename -> (delivered relative path, placement box, alignment)
COMPONENTS: dict[str, tuple[str, tuple[int, int, int, int], str]] = {
    "01-adult-creature-any_01.png": (
        "components/adult/creature/any_01.png",
        (176, 40, 336, 248),
        "full_height",
    ),
    "02-adult-creature-any_02.png": (
        "components/adult/creature/any_02.png",
        (176, 40, 336, 248),
        "full_height",
    ),
    "03-adult-creature-any_03.png": (
        "components/adult/creature/any_03.png",
        (156, 40, 356, 248),
        "full_height",
    ),
    "04-baby-creature-any_01.png": (
        "components/baby/creature/any_01.png",
        (190, 60, 322, 248),
        "full_height",
    ),
    "05-baby-creature-any_02.png": (
        "components/baby/creature/any_02.png",
        (166, 60, 346, 248),
        "full_height",
    ),
    "06-baby-creature-any_03.png": (
        "components/baby/creature/any_03.png",
        (184, 60, 328, 248),
        "full_height",
    ),
    "07-adult-face-any_happy_01.png": (
        "components/adult/face/any_happy_01.png",
        (232, 52, 280, 86),
        "center",
    ),
    "08-adult-face-any_neutral_01.png": (
        "components/adult/face/any_neutral_01.png",
        (232, 52, 280, 86),
        "center",
    ),
    "09-adult-face-any_hungry_01.png": (
        "components/adult/face/any_hungry_01.png",
        (232, 52, 280, 86),
        "center",
    ),
    "10-baby-face-any_happy_01.png": (
        "components/baby/face/any_happy_01.png",
        (224, 78, 288, 122),
        "center",
    ),
    "11-baby-face-any_neutral_01.png": (
        "components/baby/face/any_neutral_01.png",
        (224, 78, 288, 122),
        "center",
    ),
    "12-baby-face-any_hungry_01.png": (
        "components/baby/face/any_hungry_01.png",
        (224, 78, 288, 122),
        "center",
    ),
    "16-adult-back-dromedary_cross_01.png": (
        "components/adult/back/dromedary_cross_01.png",
        (225, 100, 287, 135),
        "bottom",
    ),
    "17-adult-back-aegis_cama_01.png": (
        "components/adult/back/aegis_cama_01.png",
        (185, 95, 330, 160),
        "center",
    ),
    "18-adult-detail-jopacama_01.png": (
        "components/adult/detail/jopacama_01.png",
        (205, 135, 307, 205),
        "center",
    ),
    "19-adult-detail-pudge_cama_01.png": (
        "components/adult/detail/pudge_cama_01.png",
        (300, 140, 325, 170),
        "center",
    ),
    "20-adult-detail-courier_cama_01.png": (
        "components/adult/detail/courier_cama_01.png",
        (200, 135, 312, 200),
        "center",
    ),
    "21-adult-detail-crystal_cama_01.png": (
        "components/adult/detail/crystal_cama_01.png",
        (200, 135, 312, 225),
        "center",
    ),
    "22-adult-detail-rama_01.png": (
        "components/adult/detail/rama_01.png",
        (205, 135, 305, 170),
        "center",
    ),
    "23-adult-front-invoker_cama_01.png": (
        "components/adult/front/invoker_cama_01.png",
        (225, 15, 290, 45),
        "center",
    ),
    "24-adult-face-rama_happy_01.png": (
        "components/adult/face/rama_happy_01.png",
        (232, 52, 280, 86),
        "center",
    ),
}

SCENES: dict[str, str] = {
    "13-adult-backdrop-any_01.png": "components/adult/backdrop/any_01.png",
    "14-adult-backdrop-any_02.png": "components/adult/backdrop/any_02.png",
    "15-adult-backdrop-any_03.png": "components/adult/backdrop/any_03.png",
    "25-egg.png": "egg.png",
    "26-tombstone.png": "tombstone.png",
}


def alpha_bbox(image: Image.Image) -> tuple[int, int, int, int]:
    alpha = image.getchannel("A")
    mask = alpha.point(lambda value: 255 if value > 4 else 0)
    bbox = mask.getbbox()
    if bbox is None:
        raise ValueError("component has no visible pixels")
    return bbox


def place_component(
    image: Image.Image,
    target_box: tuple[int, int, int, int],
    alignment: str,
) -> Image.Image:
    source = image.convert("RGBA").crop(alpha_bbox(image))
    left, top, right, bottom = target_box
    target_width = right - left
    target_height = bottom - top
    if alignment == "full_height":
        scale = target_height / source.height
    else:
        scale = min(target_width / source.width, target_height / source.height)
    size = (
        max(1, round(source.width * scale)),
        max(1, round(source.height * scale)),
    )
    source = source.resize(size, LANCZOS)

    x = left + (target_width - source.width) // 2
    if alignment == "full_height":
        y = top
    elif alignment == "bottom":
        y = bottom - source.height
    else:
        y = top + (target_height - source.height) // 2

    canvas = Image.new("RGBA", CANVAS, (0, 0, 0, 0))
    canvas.alpha_composite(source, (x, y))
    return canvas


def percentile_from_histogram(histogram: list[int], fraction: float) -> int:
    total = sum(histogram)
    threshold = total * fraction
    running = 0
    for value, count in enumerate(histogram):
        running += count
        if running >= threshold:
            return value
    return 255


def normalize_grayscale(image: Image.Image) -> Image.Image:
    alpha = image.getchannel("A")
    luminance = ImageOps.grayscale(image)

    histogram = [0] * 256
    for gray, opacity in zip(luminance.getdata(), alpha.getdata(), strict=True):
        if opacity > 16:
            histogram[gray] += 1

    low = percentile_from_histogram(histogram, 0.01)
    high = percentile_from_histogram(histogram, 0.99)
    if high <= low:
        high = min(255, low + 1)

    def remap(value: int) -> int:
        normalized = (value - low) / (high - low)
        return round(48 + max(0.0, min(1.0, normalized)) * 192)

    grayscale = luminance.point([remap(value) for value in range(256)])
    return Image.merge("RGBA", (grayscale, grayscale, grayscale, alpha))


def normalize_scene(source: Path, destination: Path) -> None:
    with Image.open(source) as opened:
        image = ImageOps.fit(
            opened.convert("RGBA"),
            CANVAS,
            method=LANCZOS,
            centering=(0.5, 0.5),
        )
    image.putalpha(255)
    destination.parent.mkdir(parents=True, exist_ok=True)
    image.save(destination, format="PNG", optimize=True)


def main() -> None:
    for raw_name, (relative_path, target_box, alignment) in COMPONENTS.items():
        source = KEYED_DIR / raw_name
        with Image.open(source) as opened:
            component = place_component(opened.convert("RGBA"), target_box, alignment)
        if "-creature-" in raw_name:
            component = normalize_grayscale(component)
        destination = PETS_DIR / relative_path
        destination.parent.mkdir(parents=True, exist_ok=True)
        component.save(destination, format="PNG", optimize=True)

    for raw_name, relative_path in SCENES.items():
        normalize_scene(RAW_DIR / raw_name, PETS_DIR / relative_path)

    adult_backdrops = PETS_DIR / "components" / "adult" / "backdrop"
    baby_backdrops = PETS_DIR / "components" / "baby" / "backdrop"
    baby_backdrops.mkdir(parents=True, exist_ok=True)
    for backdrop in adult_backdrops.glob("any_*.png"):
        shutil.copy2(backdrop, baby_backdrops / backdrop.name)


if __name__ == "__main__":
    main()
