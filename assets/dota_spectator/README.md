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
Only source-provided building coordinates are overlaid. League tower/barracks
bitmasks have identity and state, but no coordinates, so they appear in the
lane/tier ledger. No guessed tower or ward positions are overlaid.
