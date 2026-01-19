# Immortal Draft - Discussion and Questions

## Understanding of the Feature

Based on the spec and codebase review, here's my understanding:

**Current Flow:**
1. `/lobby` → players join (up to 14)
2. `/shuffle` → algorithm selects 10 players, balances teams, assigns roles

**Proposed Immortal Draft Flow:**
1. `/lobby` → same as current
2. `/startdraft` → selects 10 players, two captains draft teams in snake order (1-2-2-2-1)
3. Draft completes → moves to match phase (betting, then `/record`)

---

## Decisions Made

### 1. Captain Selection

**Q1.1: How should captains be auto-selected when not specified?**

✅ **DECIDED:**
- First captain: Random from eligible captains in the pool
- Second captain: Weighted random from remaining eligible captains (closer rating to first captain = higher chance of selection)

**Q1.2: What if fewer than 2 captain-eligible players are in the pool?**

✅ **DECIDED:** Bot posts a message informing the lobby that another eligible captain is needed, with instructions on how to use `/setcaptain`.

**Q1.3: Rating threshold between captains - what's the max allowed difference?**

✅ **DECIDED:** No hard threshold. The weighted random selection naturally handles this - captains with similar ratings are more likely to be paired. Large differences will be rare.

---

### 2. Player Pool Selection

**Q2.1: How are the 10 players selected from >10 in lobby?**

✅ **DECIDED:** Yes, use the same exclusion count logic as shuffle (players excluded more often get priority for inclusion).

**Q2.2: Can captains be excluded from the pool?**

✅ **DECIDED:**
- Specified captains are always included (bypass exclusion rules)
- Auto-selected captains: selection process takes exclusion into account (captains are selected from the pool of 10, not before)

---

### 3. Draft Mechanics

**Two Separate Drafts:**
| Draft | What | Timing |
|-------|------|--------|
| **Player Draft** | Captains pick players for teams (1-2-2-2-1) | Before match, in Discord |
| **Hero Draft** | Players pick heroes | In-game, in Dota 2 |

**Pre-Draft Setup Flow:**

✅ **DECIDED:**
```
Step 1: Coinflip → random winner

Step 2: Winner chooses ONE of:
        • Side (Radiant or Dire)
        • Hero Draft Order (1st or 2nd pick)

Step 3: Loser chooses the remaining option

Step 4: Lower-rated captain chooses Player Draft Order (1st/2nd)

Step 5: Player draft begins
```

**Q3.1: Snake draft order confirmed:**
```
Pick 1: Captain A picks 1 player
Pick 2: Captain B picks 1 player
Pick 3: Captain B picks 1 player
Pick 4: Captain A picks 1 player
Pick 5: Captain A picks 1 player
Pick 6: Captain B picks 1 player
Pick 7: Captain B picks 1 player
Pick 8: Captain A picks 1 player
```
(8 picks total since captains are already on their teams)

---

### 4. UI/UX in Discord

✅ **DECIDED:**

| Component | Decision |
|-----------|----------|
| **Player picking** | Buttons (name + rating) |
| **Confirmation** | No confirmation - immediate pick on click |
| **Available players** | Detailed display with preferred roles in embed |
| **Player side preference** | Live buttons - players can indicate 🟢 Radiant / 🔴 Dire preference |
| **Draft status** | Live-updating embed |

See `03 UI.md` for detailed mockups.

---

### 5. Draft State Management

**Q5.1: How should draft state be persisted?**

✅ **DECIDED:** Create new `DraftStateManager` (separate from `MatchStateManager`)

**Q5.2: What happens if draft is abandoned?**

✅ **DECIDED:** `/canceldraft` command - accessible by either captain or admin

---

### 6. Integration with Existing Systems

**Q6.1: Betting integration**

✅ **DECIDED:** Same as shuffle (15-min window after draft finalized, pool/house modes)

**Q6.2: Match recording**

✅ **DECIDED:** Same `/record` flow and voting system

**Q6.3: Role assignments**

✅ **DECIDED:** Draft does NOT assign roles - just players. Roles are assigned in-game by captains.

---

### 7. Data Model Changes

**Required:**
- `players.is_captain_eligible BOOLEAN DEFAULT 0` - new column

**Possibly needed:**
- Track who was captain for stats?

---

### 8. Command Summary

| Command | Description | Access |
|---------|-------------|--------|
| `/setcaptain yes/no` | Toggle captain eligibility | Any registered player |
| `/startdraft [captain1] [captain2]` | Begin draft | Any user (like /shuffle) |
| Buttons/UI | Pick a player (during draft) | Current captain only |
| `/canceldraft` | Abort draft | Captain or Admin |

---

## Implementation Phases

**Phase 1: Foundation**
- Add `is_captain_eligible` column + migration
- `/setcaptain` command
- Draft domain model + `DraftStateManager`

**Phase 2: Core Draft**
- `/startdraft` command
- Player pool selection (reuse exclusion logic)
- Captain auto-selection (random + weighted random)
- First/second pick choice for lower-rated captain
- Draft UI (TBD)

**Phase 3: Integration**
- Finalize draft → pending match state
- Betting integration
- `/record` works as normal

**Phase 4: Polish**
- `/canceldraft`
- Timeout handling
- Stats tracking (captain history)

---

## All Decisions Complete

| Topic | Decision |
|-------|----------|
| Captain selection | Random + weighted random (closer rating = higher chance) |
| <2 captains | Bot informs lobby, shows `/setcaptain` |
| Rating threshold | None - weighted random handles it |
| Pool selection | Same exclusion logic as shuffle |
| Specified captains | Always included in pool |
| Pre-draft setup | Coinflip → winner picks side OR hero order → loser picks other → lower-rated picks player draft order |
| Snake draft | 1-2-2-2-1 (8 picks) |
| State management | New `DraftStateManager` |
| Cancel | `/canceldraft` (captain/admin) |
| Role assignments | None - in-game by captains |
| Betting | Same as shuffle |
| Match recording | Same `/record` flow |
| UI - picking | Buttons (name + rating) |
| UI - confirmation | None (immediate) |
| UI - available players | Detailed with roles |
| UI - player preference | Live 🟢/🔴 buttons |
| Display | Live-updating embed |
| Timeout | None - manual `/canceldraft` |
| History | Use existing match storage |
