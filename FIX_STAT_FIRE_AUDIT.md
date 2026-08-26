# Strength Stats Audit: Blocks Dug Calculation

## Question
Are strength stats factored into the blocks-dug computation?

## Answer
**NO** — Strength stats are computed but never applied.

## Evidence

### Stat Effects Computation (Works Correctly)
**File: `rust/crates/cama-domain/src/dig_stats.rs:127-139`**

Strength is properly used to calculate advancement bonuses:
```rust
pub fn miner_stat_effects(stats: MinerStats) -> MinerStatEffects {
    MinerStatEffects {
        advance_min_bonus: stats.strength() / STRENGTH_MIN_ADVANCE_INTERVAL,  // / 5
        advance_max_bonus: stats.strength() / STRENGTH_MAX_ADVANCE_INTERVAL,  // / 2
        // ... other effects
    }
}
```

Where:
- `STRENGTH_MIN_ADVANCE_INTERVAL = 5` (line 14)
- `STRENGTH_MAX_ADVANCE_INTERVAL = 2` (line 12)

So the bonuses are computed as:
- `advance_min_bonus = strength / 5` (whole steps added to minimum roll)
- `advance_max_bonus = strength / 2` (whole steps added to maximum roll)

### Effects Extracted from Miner Profile (Works Correctly)
**File: `rust/crates/cama-app/src/dig_miner_runtime.rs:376-387`**

The effects are extracted into `DigMinerEffects`:
```rust
fn effects_from_stats(stats: DigMinerStats) -> DigMinerEffects {
    let effects = miner_stat_effects(
        MinerStats::new(stats.strength, stats.smarts, stats.stamina)
            .expect("normalized miner stats are non-negative"),
    );
    DigMinerEffects {
        advance_min_bonus: effects.advance_min_bonus,
        advance_max_bonus: effects.advance_max_bonus,
        // ... other effects
    }
}
```

These are part of the player's `DigMinerProfile` and displayed in `/dig miner profile`.

### Blocks Dug Calculation (DOES NOT USE STRENGTH)
**File: `rust/crates/cama-app/src/dig_runtime.rs:2522-2540`**

When constructing `DigLootModifiers` for the actual dig:
```rust
let loot_modifiers = DigLootModifiers {
    // ... many modifiers from routes, weather, gear, buffs
    advance_bonus: route_number("advance_bonus") as i64
        + weather_fx.advance_bonus
        + curse_fx.advance_bonus
        + gear_fx.advance_bonus
        + buff_fx.advance_bonus,
    advance_min: None,  // ← HARDCODED TO NONE — stat bonuses ignored
    advance_max: active_route
        .and_then(|route| route_effect(route, "advance_max_penalty"))
        .map(|penalty| (layer_at(depth_before).advance_range.1 - penalty as i64).max(1)),
    // ... other modifiers
};
```

**No references to the tunnel's stat values or the computed miner effects.**

### Blocks Actually Rolled (IGNORES STRENGTH)
**File: `rust/crates/cama-app/src/dig_loot.rs:3031-3049`**

The actual random roll happens here:
```rust
let minimum = modifiers
    .advance_min
    .unwrap_or(layer.advance_range.0)
    .max(0);
let maximum = modifiers
    .advance_max
    .unwrap_or(layer.advance_range.1)
    .max(minimum);
let base_advance = self.entropy.advance(minimum, maximum);
```

Since `advance_min` is `None` and `advance_max` only checks route penalties, the blocks are rolled from the layer's default range with no stat contribution.

## Test Coverage

**File: `rust/crates/cama-app/src/dig_stats/tests.rs`**

Tests verify the stat-to-bonus calculation works:
- `test_strength_bonuses_change_at_exact_thresholds_0_0_0` (line 182)
- `test_strength_bonuses_change_at_exact_thresholds_2_0_1` (line 192)
- `test_strength_bonuses_change_at_exact_thresholds_5_1_2` (line 202)
- etc.

These pass, confirming the stat calculation itself is correct.

But there are NO tests in `dig_runtime/tests.rs` or `dig_loot/tests.rs` that verify strength stats actually increase blocks dug during a dig action.

## Impact

A miner with 10 strength stat points should get:
- `advance_min_bonus = 10 / 5 = 2` (min +2 blocks)
- `advance_max_bonus = 10 / 2 = 5` (max +5 blocks)

But currently:
- **Min/max ranges are not modified**
- **Only `advance_bonus` (a flat number) is added, which comes from routes/weather/gear, NOT stats**

## Root Cause

When `dig_runtime.rs` prepares to call the dig loot service, it:
1. Loads the tunnel (with stat_strength, stat_smarts, stat_stamina)
2. Computes `effects_from_stats()` for display purposes
3. **Never passes these effects to the loot service**
4. The loot service has no access to miner stats and rolls from default ranges

The `tunnel` object passed to `DigLootService` contains the stat values, but `dig_runtime.rs` never extracts or passes the computed bonuses.

## Recommendation

Wire the miner stat effects through to the dig loot service by:
1. Extracting `advance_min_bonus` and `advance_max_bonus` from the tunnel stats when building `loot_modifiers`
2. Passing them as override values for `advance_min` and `advance_max`, or adding them as separate bonus fields
3. Applying them in `dig_loot.rs` when rolling blocks
4. Add regression test to verify: "A miner with 10 strength gains +2 to +5 blocks per dig"
