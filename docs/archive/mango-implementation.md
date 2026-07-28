# MangoByte Integration Implementation

Tracking document for implementing features from [mango-integration.md](./mango-integration.md).

## Phase 1: Data Enhancement (Complete)

**Goal:** Enrich existing match data display with additional fields from OpenDota.

### Tasks

- [x] Add migration for `hero_healing` and `lane_role` columns to `match_participants`
- [x] Update `MatchRepository.update_participant_stats()` to accept new fields
- [x] Update `MatchEnrichmentService.enrich_match()` to fetch and store new fields
- [x] Create `create_enriched_match_embed()` in `utils/embeds.py`
- [x] Integrate embed into `/record` command response (via auto-discovery)
- [x] Integrate embed into `/enrichmatch` command response
- [x] Add tests for enriched match display
- [x] Enhance `/matchhistory` with net_worth, tower_damage
- [x] Add `/viewmatch` command to view enriched embed for any match

### Files to Modify

| File | Changes |
|------|---------|
| `infrastructure/schema_manager.py` | Add migration for new columns |
| `repositories/match_repository.py` | Update `update_participant_stats()` signature |
| `services/match_enrichment_service.py` | Fetch `hero_healing`, `lane_role` from OpenDota |
| `utils/embeds.py` | Add `create_enriched_match_embed()` |
| `commands/match.py` | Show embed after `/record` |
| `commands/enrichment.py` | Show embed after `/enrichmatch` |
| `tests/test_services.py` | Add enrichment tests |

### OpenDota Fields Used

| Field | Source | Description |
|-------|--------|-------------|
| `hero_id` | `players[].hero_id` | Hero played |
| `kills` | `players[].kills` | Kills |
| `deaths` | `players[].deaths` | Deaths |
| `assists` | `players[].assists` | Assists |
| `gpm` | `players[].gold_per_min` | Gold per minute |
| `xpm` | `players[].xp_per_min` | XP per minute |
| `hero_damage` | `players[].hero_damage` | Total hero damage |
| `tower_damage` | `players[].tower_damage` | Total tower damage |
| `hero_healing` | `players[].hero_healing` | Total hero healing (NEW) |
| `last_hits` | `players[].last_hits` | Last hits |
| `denies` | `players[].denies` | Denies |
| `net_worth` | `players[].net_worth` or `total_gold` | End-game net worth |
| `lane_role` | `players[].lane_role` | Lane role 1-4 (NEW, parsed only) |

### Embed Design

```
Match #123 - Radiant Victory (32:45)
Score: 45-32

RADIANT (Winner)
-----------------------------------------
Hero         Player      KDA    DMG    NW
-----------------------------------------
Anti-Mage    @player1   12/3/8  45.2k  28.5k
...

DIRE
-----------------------------------------
Hero         Player      KDA    DMG    NW
-----------------------------------------
Pudge        @player6   5/8/12  22.1k  15.2k
...

Links: [OpenDota](url) | [DotaBuff](url)
```

---

## Phase 2: Player Stats Command

**Status:** Complete

- [x] Add `get_full_stats()` to `OpenDotaPlayerService`
- [x] Create `/dotastats` command
- [x] Add tests (6 new tests for distribution calculations)

---

## Phase 3: Hero Reference Commands

**Status:** Complete

- [x] Add `dotabase` dependency (+ `sqlalchemy-utils`)
- [x] Create `commands/dota_info.py` cog
- [x] Implement `/hero` command
- [x] Implement `/ability` command
- [x] Add hero/ability name autocomplete
- [x] Add tests (22 tests)

---

## Phase 4: Image Generation

**Status:** Complete

- [x] Add Pillow dependency
- [x] Create `utils/drawing.py` with:
  - `draw_matches_table()` - PNG table of recent matches
  - `draw_role_graph()` - Radar/polygon graph for hero roles
  - `draw_lane_distribution()` - Bar chart for lanes
  - `draw_attribute_distribution()` - Pie chart for hero attributes
- [x] Add `/recent` command (match table image)
- [x] Add `/rolesgraph` command (role radar graph)
- [x] Add `/lanegraph` command (lane distribution bar chart)
- [x] Add service methods for role distribution from dotabase
- [x] Add tests (17 tests for drawing functions)

---

## Phase 5: Polish & Integration

**Status:** Complete

- [x] Add hero image URL helper (`get_hero_image_url()` using Steam CDN)
- [x] Add hero color helper (`get_hero_color()` from dotabase)
- [x] Enhanced `/viewmatch` command:
  - Non-ephemeral (visible to channel)
  - Defaults to user's most recent match
  - Shows user's hero as thumbnail
  - Added hero healing column
  - MVP footer with damage stats
- [x] Enhanced `/matchhistory` command:
  - Added hero healing stat
  - Added lane role indicator
  - Hero image thumbnail
  - Footer linking to /viewmatch
- [x] Enhanced `create_enriched_match_embed()`:
  - Hero healing column
  - MVP detection and display
  - Hero image thumbnail
- [x] Updated `/help` with all new commands:
  - New "Dota 2 Stats (OpenDota)" section
  - New "Dota 2 Reference" section

---

## Phase 6: Lane Efficiency

**Status:** Complete

**Goal:** Surface lane performance data from parsed matches.

### OpenDota Laning Data (Parsed Matches Only)

| Field | Source | Description |
|-------|--------|-------------|
| `lane` | `players[].lane` | Lane (1=safe, 2=mid, 3=off) |
| `lane_role` | `players[].lane_role` | Role in lane (already stored) |
| `lane_efficiency` | `players[].lane_efficiency_pct` | Laning performance 0-100 |
| `is_roaming` | `players[].is_roaming` | Was player roaming |

### Tasks

- [x] Add migration for `lane_efficiency` column to `match_participants`
- [x] Update `MatchRepository.update_participant_stats()` to accept `lane_efficiency`
- [x] Update `MatchEnrichmentService.enrich_match()` to fetch `lane_efficiency_pct`
- [x] Add `_determine_lane_outcomes()` helper to compare lane efficiencies
- [x] Update `create_enriched_match_embed()` to show lane W/L/D outcomes
- [x] Update `/matchhistory` to show lane W/L/D outcomes
- [x] Add tests for lane outcome display (4 new tests)

### Display Format

- **Lane Won:** `Mid W`, `Safe W` - player/team won their lane
- **Lane Lost:** `Mid L`, `Off L` - player/team lost their lane
- **Lane Draw:** `Mid D`, `Safe D` - lane was even (within 5% efficiency)
- **No data:** Just lane name (`Mid`, `Safe`, `Off`) when no efficiency available

### Lane Matchup Logic

Side lanes have 2 players each (carry+support vs offlaner+support). We compare the **average efficiency** of all players in each lane:

- Radiant Safe (lane_role=1) vs Dire Off (lane_role=3)
- Radiant Mid (lane_role=2) vs Dire Mid (lane_role=2)
- Radiant Off (lane_role=3) vs Dire Safe (lane_role=1)

Win threshold: >5% average efficiency difference

### Notes

- Lane efficiency is only available for **parsed matches** (matches with replay analysis)
- Unparsed matches will show lane role without W/L/D outcome
- Both laning partners share the same outcome (W/L/D)

---

## Notes

- STRATZ API key available in `.env` - can use for additional data if needed
- OpenDota rate limit: 60 req/min (free), 1200 req/min (with key)
- Lane role values: 1=Safe, 2=Mid, 3=Off, 4=Jungle (only available for parsed matches)
- Lane efficiency: 0-100 percentage (only available for parsed matches)
