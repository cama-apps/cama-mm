# Plan: `/calibration` Command - Server Rating System Stats

## Overview
Add an ephemeral slash command that displays a snapshot of the server's rating system health, including calibration progress, rating distributions, and outliers.

## Key Glicko-2 Concepts
- **Rating Deviation (RD)**: Measures confidence in rating (350 = brand new, ~30-50 = well-calibrated)
- **Volatility**: How much a player's skill fluctuates over time
- **Uncertainty %**: `(RD / 350) * 100` - displayed to users

## Calibration Tiers (RD-based)
| Tier | RD Range | Label | Meaning |
|------|----------|-------|---------|
| Fully Calibrated | 0-75 | "Locked In" | High confidence |
| Mostly Calibrated | 76-150 | "Settling" | Good confidence |
| Partially Calibrated | 151-250 | "Developing" | Still establishing |
| Uncalibrated | 251+ | "Fresh" | New players |

## Rating Brackets (Actual Dota 2 Ranks)

Using actual Dota 2 MMR thresholds, converted to Cama rating via `mmr * 0.25`:

| Rank | MMR Range | Cama Rating |
|------|-----------|-------------|
| Immortal | 5420+ | 1355+ |
| Divine | 4620-5419 | 1155-1354 |
| Ancient | 3850-4619 | 962-1154 |
| Legend | 3080-3849 | 770-961 |
| Archon | 2310-3079 | 578-769 |
| Crusader | 1540-2309 | 385-577 |
| Guardian | 770-1539 | 192-384 |
| Herald | 0-769 | 0-191 |

Source: [Dota 2 Ranks Explained (2025)](https://www.esports.net/wiki/guides/dota-2-ranks/)

## Data Scope
- Include ALL registered players (even with 0 games)
- Show total match count for context
- **Rating Drift**: Compare current Glicko rating vs initial OpenDota MMR seed
  - `drift = glicko_rating - (initial_mmr * 0.25)`
  - Positive = outperforming pub MMR in inhouses
  - Negative = underperforming pub MMR in inhouses
  - Shows avg/median drift + biggest gainers/drops

## Embed Layout

```
Title: Rating System Health

[System Overview]
Total Players: 24 | Matches Recorded: 156
Players with Ratings: 22 | Avg Games/Player: 6.5

[Rating Distribution]
Immortal (1355+): 2 | Divine (1155-1354): 3
Ancient (962-1154): 5 | Legend (770-961): 6
Archon (578-769): 4 | Crusader (385-577): 2
Guardian (192-384): 1 | Herald (0-191): 1
Avg: 1045 | Median: 980

[Calibration Progress]
Locked In (RD ≤75): 8 players
Settling (76-150): 4 players
Developing (151-250): 6 players
Fresh (251+): 6 players
Avg Uncertainty: 42.3%

[Highest Rated]         [Most Calibrated]        [Most Volatile]
1. @Player (2456)       1. @Player (8.2%)        1. @Player (0.082)
2. @Player (2234)       2. @Player (12.9%)       2. @Player (0.078)
3. @Player (2100)       3. @Player (14.9%)       3. @Player (0.074)

[Lowest Rated]          [Least Calibrated]       [Most Experienced]
1. @Player (456)        1. @Player (100%)        1. @Player (45 games)
2. @Player (534)        2. @Player (98.6%)       2. @Player (38 games)
3. @Player (612)        3. @Player (95.4%)       3. @Player (32 games)

[Rating Drift: Seed vs Current]
How much players have moved from their initial OpenDota MMR seed.
This shows whether players are over/underperforming their pub MMR.

Avg Drift: +45 | Median Drift: +12
📈 Biggest Gainers: @Player (+245), @Player (+189), @Player (+156)
📉 Biggest Drops: @Player (-198), @Player (-167), @Player (-134)

Insight: Positive drift = performing better in inhouses than pubs

Footer: RD = Rating Deviation | Drift = Current Rating - Initial Seed
```

## Implementation

### File: `commands/info.py`

Add to existing `InfoCommands` cog (alongside `/leaderboard`).

### New Method: `compute_calibration_stats(players)`

```python
import statistics

def compute_calibration_stats(players: list, rating_system) -> dict:
    """Compute calibration statistics from all players."""
    rated = [p for p in players if p.glicko_rating is not None]

    # Buckets using all 8 Dota 2 ranks
    rating_buckets = {
        "Immortal": 0, "Divine": 0, "Ancient": 0, "Legend": 0,
        "Archon": 0, "Crusader": 0, "Guardian": 0, "Herald": 0
    }
    # Thresholds: Immortal 1355+, Divine 1155, Ancient 962, Legend 770,
    #             Archon 578, Crusader 385, Guardian 192, Herald 0
    rd_tiers = {"Locked In (≤75)": 0, "Settling (76-150)": 0, ...}

    ratings, rds, vols = [], [], []
    for p in rated:
        rd = p.glicko_rd or 350
        vol = p.glicko_volatility or 0.06
        ratings.append(p.glicko_rating)
        rds.append(rd)
        vols.append(vol)
        # Bucket logic...

    # Calculate drift for players with both initial_mmr and current rating
    # drift = current_rating - (initial_mmr * 0.25)
    drifts = []  # list of (player, drift_value)
    for p in rated:
        if p.initial_mmr is not None:
            initial_rating = p.initial_mmr * 0.25  # Convert MMR to rating scale
            drift = p.glicko_rating - initial_rating
            drifts.append((p, drift))

    drift_values = [d[1] for d in drifts]
    drifts_sorted = sorted(drifts, key=lambda x: x[1], reverse=True)

    return {
        "rating_buckets": rating_buckets,
        "rd_tiers": rd_tiers,
        "avg_rating": statistics.mean(ratings) if ratings else None,
        "median_rating": statistics.median(ratings) if ratings else None,
        "avg_uncertainty": (statistics.mean(rds) / 350 * 100) if rds else None,
        "top_rated": sorted(rated, key=lambda p: p.glicko_rating, reverse=True)[:3],
        "lowest_rated": sorted(rated, key=lambda p: p.glicko_rating)[:3],
        "most_calibrated": sorted(rated, key=lambda p: p.glicko_rd or 350)[:3],
        "least_calibrated": sorted(rated, key=lambda p: p.glicko_rd or 350, reverse=True)[:3],
        "highest_volatility": sorted(rated, key=lambda p: p.glicko_volatility or 0.06, reverse=True)[:3],
        "most_experienced": sorted(players, key=lambda p: p.wins + p.losses, reverse=True)[:3],
        # Drift stats
        "avg_drift": statistics.mean(drift_values) if drift_values else None,
        "median_drift": statistics.median(drift_values) if drift_values else None,
        "biggest_gainers": drifts_sorted[:3],  # (player, drift)
        "biggest_drops": drifts_sorted[-3:][::-1],  # bottom 3, reversed
        "total_players": len(players),
        "rated_players": len(rated),
    }
```

### Command Implementation

```python
@app_commands.command(
    name="calibration",
    description="View server rating system health and calibration stats"
)
async def calibration(self, interaction: discord.Interaction):
    # Rate limit: 2 per 30 seconds
    # Defer ephemeral
    # Fetch: player_repo.get_all()
    # Compute: compute_calibration_stats(players, rating_system)
    # Build embed with fields
    # Send ephemeral
```

## Files to Modify

| File | Change |
|------|--------|
| `commands/info.py` | Add `/calibration` command and `compute_calibration_stats()` helper |
| `repositories/match_repository.py` | Add `get_match_count()` method |
| `domain/models/player.py` | Add `initial_mmr: int | None = None` field |
| `repositories/player_repository.py` | Update `_row_to_player()` to map `initial_mmr` |

## Rating Drift Implementation

Currently `_row_to_player()` only maps `current_mmr` → `mmr`, not `initial_mmr`.

**Changes needed:**
1. Add `initial_mmr` field to Player dataclass
2. Update `_row_to_player()` to include: `initial_mmr=row["initial_mmr"]`

Then drift is computed in-memory:
```python
drift = glicko_rating - (initial_mmr * 0.25)
```

## Testing

Add `tests/test_calibration_stats.py`:
- Test empty player list
- Test bucket boundaries (exact values)
- Test sorting for top/bottom rankings
- Test with mix of calibrated/uncalibrated players
- Test rating change calculations
