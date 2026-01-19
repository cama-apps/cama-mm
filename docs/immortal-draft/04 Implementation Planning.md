# Immortal Draft - Implementation Planning

## Overview

This document outlines the implementation plan for the Immortal Draft feature.

### Feature Summary

- Captain-based player drafting as alternative to algorithmic `/shuffle`
- Pre-draft setup: coinflip → side/hero pick choices → player draft order
- Snake draft (1-2-2-2-1) with button-based UI
- Live player side preferences during draft
- Integrates with existing betting and match recording systems

---

## 1. Data Model Changes

### 1.1 New Column: `players.is_captain_eligible`

```sql
ALTER TABLE players ADD COLUMN is_captain_eligible INTEGER DEFAULT 0;
```

Add to `infrastructure/schema_manager.py` migrations.

### 1.2 Draft State (In-Memory)

New `DraftStateManager` class - no database table needed (like `MatchStateManager`).

```python
@dataclass
class DraftState:
    guild_id: int

    # Captains
    captain1_id: int          # Discord ID
    captain2_id: int          # Discord ID
    captain1_team: str        # "radiant" or "dire"
    captain2_team: str        # "radiant" or "dire"

    # Pre-draft choices
    coinflip_winner_id: int
    hero_first_pick: str      # "radiant" or "dire"

    # Player draft
    player_draft_first: int   # Discord ID of captain picking first
    available_player_ids: list[int]
    radiant_player_ids: list[int]
    dire_player_ids: list[int]

    # Draft progress
    current_pick: int         # 1-8
    picks_remaining_this_turn: int  # 1 or 2

    # Player preferences (live)
    side_preferences: dict[int, str]  # {discord_id: "radiant" | "dire"}

    # UI state
    draft_message_id: int | None
    draft_channel_id: int | None

    # Phase
    phase: str  # "coinflip" | "winner_choice" | "loser_choice" | "player_draft_choice" | "drafting" | "complete"
```

---

## 2. New Files to Create

### 2.1 Domain Layer

| File | Purpose |
|------|---------|
| `domain/models/draft.py` | `DraftState` dataclass |
| `domain/services/draft_service.py` | Pure draft logic (captain selection, pick order, validation) |

### 2.2 Services Layer

| File | Purpose |
|------|---------|
| `services/draft_state_manager.py` | In-memory draft state management (like `MatchStateManager`) |
| `services/draft_orchestrator.py` | Orchestrates draft flow, integrates with player/match services |

### 2.3 Commands Layer

| File | Purpose |
|------|---------|
| `commands/draft.py` | `/setcaptain`, `/startdraft`, `/canceldraft` commands + button views |

### 2.4 Utils (Optional)

| File | Purpose |
|------|---------|
| `utils/draft_embeds.py` | Draft-specific embed builders (or add to existing `embeds.py`) |

---

## 3. Changes to Existing Files

### 3.1 Database & Schema

| File | Changes |
|------|---------|
| `infrastructure/schema_manager.py` | Add migration for `is_captain_eligible` column |

### 3.2 Repositories

| File | Changes |
|------|---------|
| `repositories/player_repository.py` | Add `get_captain_eligible(ids)`, `set_captain_eligible(id, bool)` |
| `repositories/interfaces.py` | Add interface methods if needed |

### 3.3 Services

| File | Changes |
|------|---------|
| `services/match_service.py` | Add method to create pending match from draft result |
| `services/lobby_service.py` | May need to expose player selection logic |

### 3.4 Bot Setup

| File | Changes |
|------|---------|
| `bot.py` | Initialize `DraftStateManager`, `DraftOrchestrator`, load `commands.draft` cog |
| `config.py` | Add any draft-specific config (optional) |

---

## 4. Implementation Phases

### Phase 1: Foundation (Data + Basic Commands)

**Goal:** Get `/setcaptain` working and data model in place.

**Tasks:**

1. **Schema migration**
   - Add `is_captain_eligible` column to `players` table
   - File: `infrastructure/schema_manager.py`

2. **Repository methods**
   - `PlayerRepository.set_captain_eligible(discord_id, eligible: bool)`
   - `PlayerRepository.get_captain_eligible_players(discord_ids) -> list[int]`
   - File: `repositories/player_repository.py`

3. **`/setcaptain` command**
   - Toggle captain eligibility for the calling user
   - Ephemeral response confirming status
   - File: `commands/draft.py`

4. **Tests**
   - Repository tests for captain eligibility
   - Command tests for `/setcaptain`

**Deliverable:** Users can set themselves as captain-eligible.

---

### Phase 2: Draft State & Captain Selection

**Goal:** Implement draft state management and captain selection logic.

**Tasks:**

1. **Draft domain model**
   - `DraftState` dataclass
   - File: `domain/models/draft.py`

2. **Draft state manager**
   - `DraftStateManager` class (in-memory, guild-keyed)
   - Methods: `create_draft()`, `get_draft()`, `clear_draft()`
   - File: `services/draft_state_manager.py`

3. **Captain selection logic**
   - Random first captain from eligible
   - Weighted random second captain (closer rating = higher chance)
   - File: `domain/services/draft_service.py`

4. **Player pool selection**
   - Reuse exclusion count logic from shuffler
   - Ensure specified captains are always included
   - File: `domain/services/draft_service.py`

5. **Tests**
   - Captain selection (random + weighted)
   - Player pool selection with exclusions
   - Draft state management

**Deliverable:** Draft can be created with proper captain selection.

---

### Phase 3: Pre-Draft Setup UI

**Goal:** Implement the multi-step pre-draft flow (coinflip → choices).

**Tasks:**

1. **`/startdraft` command (initial)**
   - Validate lobby has 10+ players
   - Validate 2+ captain-eligible players (or specified captains)
   - Select player pool (10 players)
   - Select captains
   - Initiate coinflip phase
   - File: `commands/draft.py`

2. **Coinflip UI**
   - Display coinflip result
   - Winner gets buttons: "Choose Side" / "Choose Hero Pick"
   - File: `commands/draft.py` (View classes)

3. **Winner choice UI**
   - If chose side: show Radiant/Dire buttons
   - If chose hero pick: show 1st/2nd buttons
   - File: `commands/draft.py`

4. **Loser choice UI**
   - Show remaining choice (opposite of what winner picked)
   - File: `commands/draft.py`

5. **Player draft order UI**
   - Lower-rated captain chooses 1st/2nd player draft pick
   - File: `commands/draft.py`

6. **State transitions**
   - `phase` field updates through: coinflip → winner_choice → loser_choice → player_draft_choice → drafting
   - File: `services/draft_state_manager.py`

7. **Tests**
   - Pre-draft flow state transitions
   - Button interactions

**Deliverable:** Complete pre-draft setup flow working.

---

### Phase 4: Player Draft UI

**Goal:** Implement the actual drafting phase with buttons.

**Tasks:**

1. **Draft embed builder**
   - Teams side-by-side (with captains marked 👑)
   - Current turn indicator
   - Available players with roles
   - Hero pick info in header
   - File: `utils/embeds.py` or `utils/draft_embeds.py`

2. **Player pick buttons**
   - One button per available player (name + rating)
   - Only current captain can click
   - Immediate pick (no confirmation)
   - File: `commands/draft.py` (DraftView, PlayerPickButton)

3. **Player side preference buttons**
   - 🟢 Radiant / 🔴 Dire buttons
   - Only unpicked players can click
   - Updates embed in real-time
   - File: `commands/draft.py` (SidePreferenceButton)

4. **Pick logic**
   - Snake order: 1-2-2-2-1
   - Track `current_pick`, `picks_remaining_this_turn`
   - Switch captains at appropriate times
   - File: `domain/services/draft_service.py`

5. **Message updates**
   - Edit draft message after each pick
   - Edit after preference changes
   - File: `commands/draft.py`

6. **Draft completion detection**
   - After 8 picks, transition to complete phase
   - File: `services/draft_state_manager.py`

7. **Tests**
   - Pick validation (correct captain, available player)
   - Snake order logic
   - Preference updates
   - Draft completion

**Deliverable:** Full drafting flow working with live UI.

---

### Phase 5: Integration with Match System

**Goal:** Connect completed draft to betting and match recording.

**Tasks:**

1. **Create pending match from draft**
   - Convert draft result to `PendingMatchState`
   - Set `radiant_team_ids`, `dire_team_ids`
   - Set `bet_lock_until` (15 min window)
   - Set `first_pick_team` (from hero draft choice)
   - File: `services/draft_orchestrator.py`

2. **Post-draft message**
   - Show final teams
   - Betting instructions
   - Link to `/record`
   - File: `commands/draft.py`

3. **Betting integration**
   - Same as shuffle - auto-blind bets, pool mode
   - File: existing betting flow (no changes needed?)

4. **`/record` works normally**
   - Existing flow should work once pending match exists
   - Verify integration

5. **Lobby cleanup**
   - Reset lobby after draft starts (like shuffle does)
   - File: `commands/draft.py`

6. **Tests**
   - Draft → pending match creation
   - Betting flow after draft
   - Match recording after draft

**Deliverable:** Complete end-to-end flow from `/startdraft` to `/record`.

---

### Phase 6: Polish & Edge Cases

**Goal:** Handle edge cases, add `/canceldraft`, improve UX.

**Tasks:**

1. **`/canceldraft` command**
   - Only captain or admin can cancel
   - Clears draft state
   - Posts cancellation message
   - File: `commands/draft.py`

2. **Error handling**
   - Captain leaves server during draft
   - Button interaction timeout
   - Concurrent draft attempts

3. **Validation improvements**
   - Block `/startdraft` if draft already in progress
   - Block `/shuffle` if draft in progress
   - Block `/startdraft` if pending match exists

4. **UX polish**
   - Coinflip animation (optional)
   - Better error messages
   - Progress indicators

5. **Documentation**
   - Update `CLAUDE.md` with new commands
   - Update `/help` command

**Deliverable:** Production-ready feature.

---

## 5. File Structure Summary

```
New files:
├── domain/
│   ├── models/
│   │   └── draft.py              # DraftState dataclass
│   └── services/
│       └── draft_service.py      # Captain selection, pick logic
├── services/
│   ├── draft_state_manager.py    # In-memory state management
│   └── draft_orchestrator.py     # Orchestrates draft flow
└── commands/
    └── draft.py                  # Commands + View classes

Modified files:
├── infrastructure/
│   └── schema_manager.py         # Migration for is_captain_eligible
├── repositories/
│   ├── interfaces.py             # Add captain methods (optional)
│   └── player_repository.py      # Captain eligibility methods
├── services/
│   └── match_service.py          # Create pending match from draft
├── utils/
│   └── embeds.py                 # Draft embed builder
├── bot.py                        # Initialize draft services, load cog
└── config.py                     # Draft config (optional)
```

---

## 6. Testing Strategy

### Unit Tests

| Test File | Coverage |
|-----------|----------|
| `tests/test_draft_state.py` | DraftState, DraftStateManager |
| `tests/test_draft_service.py` | Captain selection, pick logic, pool selection |
| `tests/test_draft_repository.py` | Captain eligibility CRUD |

### Integration Tests

| Test File | Coverage |
|-----------|----------|
| `tests/test_draft_integration.py` | Full draft flow with real DB |
| `tests/test_draft_betting.py` | Draft → betting → settlement |

### E2E Tests

| Test File | Coverage |
|-----------|----------|
| `tests/test_e2e_draft.py` | `/startdraft` → draft → `/record` |

---

## 7. Dependencies & Order

```
Phase 1 (Foundation)
    │
    ▼
Phase 2 (State & Captain Selection)
    │
    ▼
Phase 3 (Pre-Draft UI)
    │
    ▼
Phase 4 (Player Draft UI)
    │
    ▼
Phase 5 (Match Integration)
    │
    ▼
Phase 6 (Polish)
```

Each phase builds on the previous. Phases 3 and 4 are the largest.

---

## 8. Estimated Complexity

| Phase | Complexity | Key Challenges |
|-------|------------|----------------|
| Phase 1 | Low | Just DB + simple command |
| Phase 2 | Medium | Weighted random logic |
| Phase 3 | Medium-High | Multi-step UI, state transitions |
| Phase 4 | High | Button interactions, live updates |
| Phase 5 | Medium | Integration points |
| Phase 6 | Low | Edge cases, polish |

---

## 9. Open Implementation Questions

1. **Debouncing preference updates** - If many players click preferences quickly, should we debounce message edits?

2. **Persistence on bot restart** - Draft state is in-memory. If bot restarts mid-draft, draft is lost. Is this acceptable?

3. **Thread for draft** - Should draft happen in a thread (like lobby shuffle messages)?

4. **Excluded players notification** - When >10 in lobby, should excluded players be notified?

---

## 10. Config Options (Optional)

```python
# config.py additions (all optional)
DRAFT_ENABLED = True                    # Feature flag
DRAFT_COINFLIP_DELAY_SECONDS = 2        # Suspense for coinflip
CAPTAIN_RATING_WEIGHT_FACTOR = 100      # For weighted random (higher = more weight to similar ratings)
```
