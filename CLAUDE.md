# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Cama Balanced Shuffle is a Discord bot for Dota 2 inhouse leagues that implements balanced team shuffling using the Glicko-2 rating system. Features include player registration, team balancing, match recording, betting system (jopacoin), and leaderboards.

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
```

## Architecture

**Layered Architecture:** Domain → Services → Repositories → Database

```
bot.py                    # Main entry point, Discord bot initialization
config.py                 # Environment variable configuration
commands/                 # Discord slash commands organized by feature
domain/
  models/                 # Pure domain models (Player, Team, Lobby)
  services/               # Domain services (role assignment, team balancing)
repositories/             # Data access layer with interfaces
  interfaces.py           # Abstract interfaces (IPlayerRepository, etc.)
services/                 # Application services orchestrating repos + domain
infrastructure/
  schema_manager.py       # SQLite schema creation and migrations
utils/                    # Helpers (embeds, formatting, rate limiting)
tests/                    # 41 test files total
```

**Key Patterns:**
- Repository Pattern: All data access through interfaces in `repositories/interfaces.py`
- Dependency Injection: Services receive repositories as constructor arguments
- Guild-Aware: All features support multi-guild operation with `guild_id` tracking

## Key Modules

- **BalancedShuffler** (`shuffler.py`): Team balancing algorithm minimizing skill difference using Glicko-2 ratings with role assignment optimization
- **CamaRatingSystem** (`rating_system.py`): Converts OpenDota MMR (0-12000) to Glicko-2 scale (0-3000)
- **MatchService** (`services/match_service.py`): Core orchestration for team shuffling and match recording
- **BettingService** (`services/betting_service.py`): Jopacoin wagering with house multiplier payouts

## Configuration

**Required:** `DISCORD_BOT_TOKEN`

**Optional:**
- `ADMIN_USER_IDS` - Comma-separated Discord user IDs for admin commands
- `DB_PATH` - Database file path (default: `cama_shuffle.db`)
- `OPENDOTA_API_KEY` - API key for higher rate limits
- `LOBBY_READY_THRESHOLD` - Min players to shuffle (default: 10)
- `OFF_ROLE_MULTIPLIER` / `OFF_ROLE_FLAT_PENALTY` - Role assignment tuning

## Testing

- **Test fixtures** in `tests/conftest.py`: Use `temp_db_path` for database isolation
- **Async testing**: All Discord interactions are async; tests use pytest-asyncio
- **Guild ID**: Tests use `guild_id=None` for default behavior

## Important Notes

- **Single Instance Lock**: Bot enforces one running instance via `.bot.lock` file
- **Rating System**: Uses Glicko-2, not simple MMR; initial RD=350.0, volatility=0.06
- **5 Roles**: 1=carry, 2=mid, 3=offlane, 4=support, 5=hard_support (stored as strings)
- **OpenDota Integration**: Rate-limited API client in `opendota_integration.py`
