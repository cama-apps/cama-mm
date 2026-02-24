# CLAUDE.md

## Git Commits

When committing, do not include the Co-Authored-By trailer.

## Project Overview

Cama Balanced Shuffle is a Discord bot for Dota 2 inhouse leagues. It implements:
- **Balanced team shuffling** using Glicko-2 ratings with role-aware optimization
- **Captain's Draft mode** with coinflip, side/pick selection, and snake draft
- **Dual rating systems**: Glicko-2 (primary) and OpenSkill Plackett-Luce (fantasy-weighted)
- **Player registration** with OpenDota MMR integration
- **Match recording** with rating updates, pairwise statistics, and fantasy points
- **Jopacoin betting system** with house/pool modes, leverage (2x-5x), debt, and bankruptcy (unified for shuffle and draft)
- **Prediction markets** for yes/no outcomes with resolution voting and payouts
- **Jopacoin economy**: Loans, nonprofit disbursements, shop purchases, tipping, Wheel of Fortune
- **Match enrichment** via OpenDota/Valve APIs for detailed stats (K/D/A, heroes, GPM, lane outcomes, fantasy)
- **Dota 2 reference** commands for hero/ability lookup (via dotabase)
- **Stats visualization** with image generation (radar graphs, bar charts, match tables, wheel animations)
- **AI features** (optional): Flavor text generation, natural language SQL queries via Cerebras LLM

## Commands

```bash
# Create venv and install dependencies
uv venv
uv sync

# Run the bot
uv run python bot.py

# Run all tests (parallel)
uv run pytest -n auto

# Run specific test file
uv run pytest tests/test_e2e_workflow.py -v

# Run single test
uv run pytest tests/test_betting_service.py::TestBettingService::test_place_bet -v

# Restart the bot (use anchored pattern to avoid killing the shell itself)
pkill -f "^uv run python bot.py$" 2>/dev/null || true; sleep 1; nohup uv run python bot.py > /tmp/bot.log 2>&1 &

# Check bot logs
tail -f /tmp/bot.log
```

## Architecture

**Layered Architecture:** Domain → Services → Repositories → Database

## Key Patterns

### Repository Pattern
All data access goes through interfaces in `repositories/interfaces.py`. Services receive repositories via constructor injection:

### Guild-Aware Design (Multi-Guild Isolation)
All data is segmented by `guild_id` to support running the bot in multiple Discord servers with complete isolation. Each guild has its own players, ratings, balances, matches, and economy.

**Composite Primary Keys:**
Most tables use `(discord_id, guild_id)` as the composite primary key:
- `players` - Player data is per-guild (same Discord user can have different ratings in different guilds)
- `matches` - Matches belong to a specific guild
- `bets`, `predictions`, `lobbies` - All guild-scoped
- `rating_history`, `recalibration_state`, `bankruptcy_state`, `loan_state` - Per-guild tracking

**guild_id Normalization:**
- `guild_id=None` is normalized to `0` in all repositories via `normalize_guild_id()`
- Use `guild_id=None` or `guild_id=0` for DMs or single-guild tests
- Commands extract guild_id via: `guild_id = interaction.guild.id if interaction.guild else None`

**Cross-Guild Exceptions:**
These intentionally remain cross-guild:
- `player_steam_ids` - Steam accounts are globally unique, not per-guild
- Steam ID lookups (`get_steam_ids`, `get_discord_id_by_steam_id`) - Global for match discovery

### Atomic Database Operations
Critical operations use `BEGIN IMMEDIATE` for write locks:

## Domain Models

### Player (`domain/models/player.py`)

### Team (`domain/models/team.py`)

### Lobby (`domain/models/lobby.py`)

### DraftState (`domain/models/draft.py`)

## Key Services

### MatchService (`services/match_service.py`)

### BettingService (`services/betting_service.py`)

### PredictionService (`services/prediction_service.py`)

### AIService (`services/ai_service.py`)

## Database Schema (Key Tables)

### player_steam_ids (Multi-Steam ID Support)


## Slash Commands Quick Reference
If you need more information on Slash Commands, read `README.md`

## Testing

**All new functionality must include tests.** Run `uv run pytest -n auto` before committing.

### Test Types
- **Unit tests**: Single method/class in isolation (`test_repositories.py`)
- **Integration tests**: Services + repositories + DB (`test_betting_service.py`)
- **E2E tests**: Complete workflows (`test_e2e_workflow.py`)

### Key Fixtures (conftest.py)
```python
@pytest.fixture
def temp_db_path():
    """Temporary database file path (no schema)"""

@pytest.fixture
def repo_db_path():
    """Temporary database WITH initialized schema"""

@pytest.fixture
def player_repository(repo_db_path):
    """Ready-to-use PlayerRepository"""

@pytest.fixture
def sample_players():
    """12 Player objects for shuffler tests"""
```

### Conventions
- Use `repo_db_path` fixture (not `temp_db_path`) for repository tests
- Use `guild_id=None` or `guild_id=0` for single-guild tests
- Mock external APIs (OpenDota, Discord) in integration tests

## Configuration
See `config.py` for the full list (50+ options). See `README.md` for high level info.

## Common Modification Patterns

### Adding a New Slash Command
1. Create or edit file in `commands/`
2. Use `@app_commands.command()` decorator
3. Inject services via `interaction.client.<service>`
4. Add rate limiting: `@app_commands.checks.cooldown(rate, per)`
5. Add tests in `tests/test_<feature>_commands.py`

### Adding a New Service
1. Create `services/<name>_service.py`
2. Accept repositories via constructor injection
3. Add interface if needed in `repositories/interfaces.py`
4. Initialize in `bot.py::_init_services()`
5. Expose on bot object: `bot.<service> = <service>`

### Adding a Database Column
1. Add migration in `infrastructure/schema_manager.py::_get_migrations()`
2. Use `ALTER TABLE ADD COLUMN IF NOT EXISTS` pattern
3. Update repository to read/write new column
4. Update domain model if applicable

### Adding a New Repository
1. Define interface in `repositories/interfaces.py`
2. Implement in `repositories/<name>_repository.py`
3. Extend `BaseRepository` for connection management
4. Initialize in `bot.py::_init_services()`
