# Cama Pets Generation Prompt Set

Mode: built-in ImageGen. Component sources used a flat `#FF00FF` chroma-key background and local alpha removal. All prompts also prohibited text, watermarks, UI, borders, extra objects, floor shadows, and background variation.

## Shared component direction

Painterly pixel-art-adjacent art for a layered Discord collectible-pet card, with dark underground fantasy RPG accents and cute protective companion appeal. Use a 16:9 landscape canvas and place only the named component in its supplied normalized card zone. Preserve enormous empty chroma-key space. Never include a full creature in face or species-overlay assets.

## Creature bodies

All six bodies were prompted as strictly neutral grayscale, faceless camel-llama hybrids with full dark-to-light value range:

- `adult/creature/any_01.png`: stocky and fluffy; sturdy legs, plump rounded wool body, medium neck, round head, small upright ears.
- `adult/creature/any_02.png`: lean and elegant; slender legs, oval body, long graceful neck, small head, neat ears.
- `adult/creature/any_03.png`: extra round and woolly; short hidden legs, huge wool body, head sunk into fluff.
- `baby/creature/any_01.png`: chibi standing; oversized blank head, small plump body, no neck, stubby legs, big ears.
- `baby/creature/any_02.png`: chibi egg silhouette; huge blank head, tiny tucked body, minuscule legs, floppy ears.
- `baby/creature/any_03.png`: chibi puppy sit; visible haunches, tiny forelegs, tilted blank head.

## Faces

Every face prompt required floating features only—eyes, pale oval muzzle, nostril dots, and mouth—with no head, fur, ears, neck, or body:

- Happy: large sparkling dark eyes, open curved smile, two soft pink blush marks.
- Neutral: half-lidded camelid eyes and a flat little mouth.
- Hungry: drooping eyes, downturned mouth, exactly one tear below the left eye.
- Adult and baby versions used their respective head zones and chibi scale.
- `rama_happy_01.png`: big highlighted eyes under thick angled scowling brows, pale muzzle, reluctant flat mouth, no blush or smile.

## Backdrops

- `any_01.png`: empty cozy rustic stable, wooden beams and sparse straw, one warm lantern, deep blue-purple shadow, amber accents, clear center.
- `any_02.png`: empty twilight desert oasis, distant dunes, early stars, palms confined to the edges, clear center.
- `any_03.png`: cave mouth viewed from inside, dark stone edge framing, moonlit opening, faint blue crystals, clear center.

## Species overlays

- `dromedary_cross_01.png`: one modest sandy wool hump with cream top highlight.
- `aegis_cama_01.png`: golden protective half-dome shell arc with pale inner ring and warm rim glow.
- `jopacama_01.png`: exactly eight separate faint gold coin-shaped spiral whorls in a balanced body pattern.
- `pudge_cama_01.png`: exactly one tiny dull-steel J-shaped hook with a subtle glint.
- `courier_cama_01.png`: paired brown saddlebags with lighter flaps, connecting straps, and one tiny red-orange flag.
- `crystal_cama_01.png`: icy blue-white wool glints plus a short fringe of 5-7 belly icicles.
- `rama_01.png`: one undersized crimson royal blanket with restrained ornate gold trim.
- `invoker_cama_01.png`: three small glowing orbs—ice blue, deep blue-violet, ember orange—with bright cores and specular dots.

The Invoker overlay used a second attempt to move the center orb from magenta-adjacent violet to deep blue-violet so its color survived chroma-key removal.

## Full cards

- `egg.png`: exactly one mysterious speckled cream egg in a straw nest, warm overhead lantern glow, dark cozy stable, no crack or hint of a creature.
- `tombstone.png`: small rounded gravestone on a grassy twilight mound with flowers and stars; broad front panel completely blank and smooth for later name rendering.

