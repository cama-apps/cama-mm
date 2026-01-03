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
tests/                    # Test files (unit, integration, e2e)
```

**Key Patterns:**
- Repository Pattern: All data access through interfaces in `repositories/interfaces.py`
- Dependency Injection: Services receive repositories as constructor arguments
- Guild-Aware: All features support multi-guild operation with `guild_id` tracking

## Key Modules

- **BalancedShuffler** (`shuffler.py`): Team balancing algorithm minimizing skill difference using Glicko-2 ratings with role assignment optimization
- **CamaRatingSystem** (`rating_system.py`): Converts OpenDota MMR (0-12000) to Glicko-2 scale (0-3000)
- **MatchService** (`services/match_service.py`): Core orchestration for team shuffling and match recording
- **BettingService** (`services/betting_service.py`): Jopacoin wagering with two modes: house (1:1 fixed odds) or pool (parimutuel user-determined odds). Supports leverage betting (2x, 3x, 5x) with debt mechanics.
- **GarnishmentService** (`services/garnishment_service.py`): Handles debt repayment by garnishing winnings when players have negative balances

## Configuration

**Required:** `DISCORD_BOT_TOKEN`

**Optional:**
- `ADMIN_USER_IDS` - Comma-separated Discord user IDs for admin commands
- `DB_PATH` - Database file path (default: `cama_shuffle.db`)
- `OPENDOTA_API_KEY` - API key for higher rate limits
- `LOBBY_READY_THRESHOLD` - Min players to shuffle (default: 10)
- `OFF_ROLE_MULTIPLIER` / `OFF_ROLE_FLAT_PENALTY` - Role assignment tuning

**Betting/Debt Configuration:**
- `LEVERAGE_TIERS` - Comma-separated leverage options (default: `2,3,5`)
- `MAX_DEBT` - Maximum debt from leveraged bets (default: 500)
- `GARNISHMENT_PERCENTAGE` - Portion of winnings applied to debt (default: 1.0 = 100%)

## Testing

**All new functionality must include tests.** Run `uv run pytest -n auto` before committing.

- **Unit tests**: For domain logic (shuffler, rating system, lobby)
- **Integration tests**: For services and repositories
- **E2E tests**: For complete workflows (see `tests/test_e2e_*.py`)

**Conventions:**
- Use `temp_db_path` fixture for database isolation
- Use `guild_id=None` for default behavior in tests
- Follow existing patterns in similar test files

## Important Notes

- **Single Instance Lock**: Bot enforces one running instance via `.bot.lock` file
- **Rating System**: Uses Glicko-2, not simple MMR; initial RD=350.0, volatility=0.06
- **5 Roles**: 1=carry, 2=mid, 3=offlane, 4=support, 5=hard_support (stored as strings)
- **OpenDota Integration**: Rate-limited API client in `opendota_integration.py`
- **Betting Modes**: `/shuffle betting_mode:` accepts "house" (default, 1:1 payouts) or "pool" (parimutuel, odds from bet distribution)
- **Leverage Betting**: `/bet` supports leverage (2x, 3x, 5x) multiplying effective bet. Losses can push players into debt up to `MAX_DEBT`. Debtors have 100% of winnings garnished until debt is repaid.
- **Debt Commands**: `/balance` shows debt info, `/paydebt` allows helping another player pay down their debt (requires positive balance)
