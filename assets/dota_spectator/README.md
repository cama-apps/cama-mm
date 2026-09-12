# Spectator map asset

`map-7.40.png` is a 640×640 PNG conversion of OpenDota's 7.40 minimap:
https://github.com/odota/web/blob/1b7ce1ca467403ed6d0ca871e7802924d91a31f5/public/assets/images/dota2/map/detailed_740.jpg

Original SHA-256: `0cf2d0f886f4c007da2cd8cbe1a18d6ea3835d296db3d54cc9e74e14688e8237`.
Bundled SHA-256: `8eebb1cd0b2519b8147487cd72cd481e0a3c503ac12725aae10b9c0893d4922f`.

The OpenDota project distributes its source under MIT (included in
`OPENDOTA-LICENSE.txt`). Dota 2 terrain/artwork remains Valve Corporation's
property; this is not a claim that Valve's game assets are MIT licensed.
The asset was downloaded without credentials from GitHub's raw-content CDN.
It is bundled so production does not fetch map art or depend on another website.

Conversion used the host's native GdkPixbuf decoder/scaler (bilinear), preserving
the entire square image with no crop. No Python imaging package or AI-generated
terrain is used. The one-time conversion tool is not a runtime dependency.

## Projection and limitations

The matching revision's `src/utility.tsx::gameCoordToUV` maps OpenDota's
128-unit cells to image U/V: `u = cell_x - 64`, `v = 127 - (cell_y - 64)`.
`TeamfightMap` scales these coordinates by image width / 127. With Source 2's
world-origin cell offset 128, the continuous transform is
`u = (world_x / 128 + 64) / 127`, `v = (63 - world_y / 128) / 127`.
The corresponding crop bounds are -8192 to +8064. The renderer preserves that
projection and drops out-of-crop coordinates rather than pinning them to edges.

This is a static **7.40 terrain reference**, not a live rendering of trees,
terrain changes, wards, vision, runes, or neutral camps. Terrain can differ on
future patches; update the pinned asset and revalidate alignment when it does.
Static objective artwork in the background does not indicate live status.
Standing towers and barracks are overlaid only when their identity and state
are present in the current feed. Explicit building coordinates take precedence.
League bitmasks supply identity/state but no coordinates; those entries use
the static OpenDota building layout described below. Destroyed and unknown
structures do not receive a standing-building marker. No ward positions are
inferred.

## Static building marker layout

Lane tower/barracks anchors come from the same pinned OpenDota revision:
[buildingData733.ts](https://github.com/odota/web/blob/1b7ce1ca467403ed6d0ca871e7802924d91a31f5/src/components/Match/BuildingMap/buildingData733.ts).
Despite that filename, the revision's
[BuildingMap.tsx](https://github.com/odota/web/blob/1b7ce1ca467403ed6d0ca871e7802924d91a31f5/src/components/Match/BuildingMap/BuildingMap.tsx)
selects this layout for all matches from 7.33 onward, including 7.40. Its
[DotaMap.tsx](https://github.com/odota/web/blob/1b7ce1ca467403ed6d0ca871e7802924d91a31f5/src/components/DotaMap/DotaMap.tsx)
selects the same `detailed_740.jpg` image bundled here for 7.40 games.

The upstream values are CSS top-left percentages. On its 300px building map,
OpenDota displays square tower sprites at 16px and barracks sprites at 12px.
Our centered lane geometry therefore adds half the original sprite size in each
axis: `8 / 300` of map width/height for towers and `6 / 300` for barracks.
The normalized marker center is converted through the inverse projection
above so the renderer uses a single coordinate transform at every image size.

These are **approximate static UI positions**, not coordinates measured from
the current match. The upstream layout predates minor subsequent tower moves;
it communicates side, lane, and tier rather than exact attack range. Revalidate
it when updating terrain. Live masks determine presence; static coordinates
never establish health, visibility, a destruction time, or whether a building
exists in a custom map. The source code is covered by the bundled OpenDota MIT
notice; no additional map or marker images are downloaded at runtime.

### Ancient and tier 4 landmarks

The older upstream core layout placed tier 4 markers near the outer base wall
on this asset. Six core centers are therefore manually aligned to the bundled
640×640 terrain reference instead. Coordinates below are marker-center pixels
from the image's upper-left, before rendering or scaling:

| Team | Ancient | Upper tier 4 | Lower tier 4 |
|---|---|---|---|
| Radiant | (82, 526) | (99, 501) | (124, 525) |
| Dire | (546, 112) | (514, 117) | (538, 141) |

These are reviewed **map-aligned approximations**, not Valve entity coordinates
or values attributed to OpenDota. Each tier 4 pair is inside the base, on the
mid-lane side of its Ancient and closer than the mid barracks. Lane tower and
barracks anchors retain the sourced layout. Explicit live coordinates continue
to take precedence.

The league feed has no Ancient status bit. An Ancient icon is a static map
landmark unless an explicit source entry provides its state; its presence must
not be interpreted as proof of current health, invulnerability, or survival.
The tier 4 masks still independently determine whether each tier 4 is drawn.
