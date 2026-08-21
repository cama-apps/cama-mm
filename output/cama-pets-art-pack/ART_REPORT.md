# Cama Pets Art Pack Report

- Generation mode: built-in ImageGen
- Unique generated artworks: 26
- Delivered PNG files: 29 (the three adult backdrops are also copied byte-for-byte to the required baby paths)
- Final format: 512x288 RGBA PNG
- Needs human review: none

| Filename | Attempts used | Checklist result |
|---|---:|---|
| `assets/pets/components/adult/creature/any_01.png` | 1 | PASS |
| `assets/pets/components/adult/creature/any_02.png` | 1 | PASS |
| `assets/pets/components/adult/creature/any_03.png` | 1 | PASS |
| `assets/pets/components/baby/creature/any_01.png` | 1 | PASS |
| `assets/pets/components/baby/creature/any_02.png` | 1 | PASS |
| `assets/pets/components/baby/creature/any_03.png` | 1 | PASS |
| `assets/pets/components/adult/face/any_happy_01.png` | 1 | PASS |
| `assets/pets/components/adult/face/any_neutral_01.png` | 1 | PASS |
| `assets/pets/components/adult/face/any_hungry_01.png` | 1 | PASS |
| `assets/pets/components/baby/face/any_happy_01.png` | 1 | PASS |
| `assets/pets/components/baby/face/any_neutral_01.png` | 1 | PASS |
| `assets/pets/components/baby/face/any_hungry_01.png` | 1 | PASS |
| `assets/pets/components/adult/backdrop/any_01.png` | 1 | PASS |
| `assets/pets/components/adult/backdrop/any_02.png` | 1 | PASS |
| `assets/pets/components/adult/backdrop/any_03.png` | 1 | PASS |
| `assets/pets/components/baby/backdrop/any_01.png` | 1 source render (copy) | PASS |
| `assets/pets/components/baby/backdrop/any_02.png` | 1 source render (copy) | PASS |
| `assets/pets/components/baby/backdrop/any_03.png` | 1 source render (copy) | PASS |
| `assets/pets/components/adult/back/dromedary_cross_01.png` | 1 | PASS |
| `assets/pets/components/adult/back/aegis_cama_01.png` | 1 | PASS |
| `assets/pets/components/adult/detail/jopacama_01.png` | 1 | PASS |
| `assets/pets/components/adult/detail/pudge_cama_01.png` | 1 | PASS |
| `assets/pets/components/adult/detail/courier_cama_01.png` | 1 | PASS |
| `assets/pets/components/adult/detail/crystal_cama_01.png` | 1 | PASS |
| `assets/pets/components/adult/detail/rama_01.png` | 1 | PASS |
| `assets/pets/components/adult/front/invoker_cama_01.png` | 2 | PASS |
| `assets/pets/components/adult/face/rama_happy_01.png` | 1 | PASS |
| `assets/pets/egg.png` | 1 | PASS |
| `assets/pets/tombstone.png` | 1 | PASS |

## Verification

- Every delivered PNG is exactly 512x288 and RGBA.
- Component corners are transparent; backdrops and full cards are fully opaque.
- All six shared creature bodies are pure grayscale, normalized across approximately `#303030` to `#F0F0F0`.
- Every component alpha bounding box is inside its registered compositor zone.
- Baby backdrop copies are byte-identical to the adult originals.
- Adult species and baby mood contact sheets were rendered through the repository's real `utils.pet_compositor` for visual registration review.

