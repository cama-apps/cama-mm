# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Cama Balanced Shuffle is a Discord bot for Dota 2 inhouse leagues. It implements:
- **Balanced team shuffling** using Glicko-2 ratings with role-aware optimization
- **Player registration** with OpenDota MMR integration
- **Match recording** with rating updates and pairwise statistics
- **Jopacoin betting system** with house/pool modes, leverage (2x-5x), debt, and bankruptcy
- **Match enrichment** via OpenDota API for detailed stats (K/D/A, heroes, GPM, lane outcomes)
- **Dota 2 reference** commands for hero/ability lookup (via dotabase)
- **Stats visualization** with image generation (radar graphs, bar charts, match tables)

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
bot.py                    # Entry point, Discord event handlers, service initialization
config.py                 # Environment configuration (all settings centralized here)
shuffler.py               # BalancedShuffler - team balancing algorithm
rating_system.py          # CamaRatingSystem - Glicko-2 rating management
opendota_integration.py   # OpenDotaAPI - rate-limited external API client

commands/                 # Discord slash commands (9 cog modules)
├── match.py              # /shuffle, /record
├── registration.py       # /register, /setroles, /stats
├── lobby.py              # /lobby, /kick, /resetlobby
├── betting.py            # /bet, /mybets, /balance, /paydebt, /bankruptcy
├── info.py               # /help, /leaderboard
├── advstats.py           # /pairwise, /matchup, /rebuildpairings
├── enrichment.py         # /enrichmatch, /profile, /matchhistory, /viewmatch,
│                         # /dotastats, /recent, /rolesgraph, /lanegraph, /autodiscover
├── dota_info.py          # /hero, /ability (dotabase reference commands)
└── admin.py              # /addfake, /resetuser, /sync

domain/
├── models/               # Pure domain models (no DB dependencies)
│   ├── player.py         # Player dataclass with ratings, roles, balance
│   ├── team.py           # Team with 5 players, role assignments, value calc
│   └── lobby.py          # Lobby state + LobbyManager for lifecycle
└── services/             # Pure domain logic (no side effects)
    ├── role_assignment_service.py   # Optimal role assignment algorithms
    └── team_balancing_service.py    # Team value and matchup scoring

services/                 # Application services (orchestrate repos + domain)
├── match_service.py      # Core: shuffle, record, voting, rating updates
├── player_service.py     # Registration, role management, stats
├── betting_service.py    # Bet placement, settlement, rewards
├── lobby_service.py      # Lobby management, embed generation
├── garnishment_service.py    # Debt repayment from winnings
├── bankruptcy_service.py     # Bankruptcy declaration and penalties
├── match_enrichment_service.py   # OpenDota match data enrichment
├── match_discovery_service.py    # Auto-discover Dota match IDs
├── opendota_player_service.py    # Player profile fetching
├── match_state_manager.py        # In-memory pending match state
└── permissions.py        # Admin permission checking

repositories/             # Data access layer
├── interfaces.py         # Abstract interfaces (IPlayerRepository, etc.)
├── base_repository.py    # Connection management, context managers
├── player_repository.py  # Player CRUD, balance, ratings, steam_id
├── match_repository.py   # Match recording, enrichment, participants
├── bet_repository.py     # Bet placement, settlement (atomic operations)
├── lobby_repository.py   # Lobby state persistence
├── pairings_repository.py    # Pairwise teammate/opponent stats
└── guild_config_repository.py    # Per-guild configuration

infrastructure/
└── schema_manager.py     # SQLite schema creation and 18 migrations

utils/
├── embeds.py             # Discord embed builders (lobby, match, enriched stats)
├── formatting.py         # Role emojis, betting display, pool odds
├── rate_limiter.py       # Token-bucket rate limiting for commands
├── hero_lookup.py        # Hero ID → name, image URL, color (via dotabase)
├── drawing.py            # Image generation (Pillow): match tables, radar graphs, bar charts
├── interaction_safety.py # Safe defer/followup for Discord interactions
└── debug_logging.py      # JSONL debug tracing (optional)

tests/                    # Test files organized by type
├── conftest.py           # Shared fixtures (temp_db_path, repositories)
├── conftest_e2e.py       # E2E fixtures (MockDiscordUser, helpers)
├── test_repositories.py  # Repository unit tests
├── test_services.py      # Service integration tests
├── test_betting_service.py   # Comprehensive betting tests
├── test_e2e_*.py         # End-to-end workflow tests
└── test_*.py             # Feature-specific tests
```

## Key Patterns

### Repository Pattern
All data access goes through interfaces in `repositories/interfaces.py`. Services receive repositories via constructor injection:
```python
class MatchService:
    def __init__(self, player_repo: IPlayerRepository, match_repo: IMatchRepository, ...):
```

### Guild-Aware Design
All features support multi-guild operation. Guild ID is tracked in:
- `pending_matches.guild_id`
- `bets.guild_id`
- `guild_config.guild_id`
Use `guild_id=None` (normalized to 0) for DMs or tests.

### Atomic Database Operations
Critical operations use `BEGIN IMMEDIATE` for write locks:
- `place_bet_atomic()` - prevents double-spending
- `settle_pending_bets_atomic()` - ensures consistent payouts
- `pay_debt_atomic()` - atomic fund transfers

### Service Dependencies
```
MatchService
├── IPlayerRepository
├── IMatchRepository
├── TeamBalancingService → RoleAssignmentService
├── CamaRatingSystem
├── BalancedShuffler
├── BettingService (optional)
│   ├── BetRepository
│   ├── PlayerRepository
│   ├── GarnishmentService
│   └── BankruptcyService
└── IPairingsRepository (optional)
```

## Domain Models

### Player (`domain/models/player.py`)
```python
@dataclass
class Player:
    name: str
    mmr: int | None              # OpenDota MMR (0-12000)
    wins: int = 0
    losses: int = 0
    preferred_roles: list[str]   # ["1", "2", "3", "4", "5"]
    main_role: str | None
    glicko_rating: float | None  # Cama rating (0-3000)
    glicko_rd: float | None      # Rating deviation (uncertainty)
    glicko_volatility: float | None
    discord_id: int | None
    jopacoin_balance: int = 0

    def get_value(use_glicko=True) -> float  # For team balancing
    def has_role(role: str) -> bool          # Check role preference
```

### Team (`domain/models/team.py`)
```python
class Team:
    ROLES = ["1", "2", "3", "4", "5"]  # Carry, Mid, Offlane, Soft/Hard Support
    TEAM_SIZE = 5

    players: list[Player]
    role_assignments: list[str] | None

    def get_team_value(use_glicko, off_role_multiplier) -> float
    def get_all_optimal_role_assignments() -> list[list[str]]  # LRU cached
    def get_off_role_count() -> int
```

### Lobby (`domain/models/lobby.py`)
```python
class Lobby:
    lobby_id: int
    players: set[int]         # Discord IDs
    status: str               # "open" or "closed"

    def is_ready(min_players=10) -> bool
    def add_player(discord_id) -> bool
    def remove_player(discord_id) -> bool

class LobbyManager:
    # Manages lobby lifecycle with persistence via ILobbyRepository
    def get_or_create_lobby(creator_id) -> Lobby
    def join_lobby(discord_id, max_players=12) -> bool
    def reset_lobby() -> None
```

## Key Services

### MatchService (`services/match_service.py`)
Core orchestrator for matches. Thread-safe via `_recording_lock`.

```python
# Shuffle players into balanced teams
shuffle_players(player_ids, guild_id, betting_mode) -> dict

# Record match result (handles voting, ratings, bets)
record_match(winning_team, guild_id, dotabuff_match_id) -> dict

# Voting system for non-admin match recording
add_record_submission(guild_id, user_id, result, is_admin) -> dict
can_record_match(guild_id) -> bool  # Checks vote threshold
```

### BettingService (`services/betting_service.py`)
Handles jopacoin wagering with two modes:
- **House mode**: 1:1 fixed odds
- **Pool mode**: Parimutuel (odds from bet distribution)

```python
place_bet(guild_id, discord_id, team, amount, pending_state, leverage) -> None
settle_bets(match_id, guild_id, winning_team, pending_state) -> dict
award_participation(player_ids) -> dict  # 1 jopacoin per game
award_win_bonus(winning_ids) -> dict     # JOPACOIN_WIN_REWARD per win
```

### PlayerService (`services/player_service.py`)
```python
register_player(discord_id, username, steam_id) -> dict  # Fetches MMR from OpenDota
set_roles(discord_id, roles) -> None
get_stats(discord_id) -> dict  # rating, uncertainty, win_rate, balance
```

## Database Schema (Key Tables)

### players
```sql
discord_id INTEGER PRIMARY KEY
discord_username TEXT NOT NULL
glicko_rating REAL, glicko_rd REAL, glicko_volatility REAL
preferred_roles TEXT  -- JSON array ["1", "2"]
jopacoin_balance INTEGER DEFAULT 3
exclusion_count INTEGER DEFAULT 0
steam_id INTEGER UNIQUE
```

### matches
```sql
match_id INTEGER PRIMARY KEY AUTOINCREMENT
team1_players TEXT, team2_players TEXT  -- JSON arrays (Radiant/Dire)
winning_team INTEGER  -- 1=Radiant, 2=Dire
valve_match_id INTEGER  -- For enrichment
enrichment_source TEXT  -- 'manual' or 'auto'
```

### bets
```sql
guild_id INTEGER NOT NULL DEFAULT 0
discord_id INTEGER NOT NULL
team_bet_on TEXT  -- 'radiant' or 'dire'
amount INTEGER
leverage INTEGER DEFAULT 1  -- 2x, 3x, 5x multipliers
bet_time INTEGER  -- Unix timestamp (indexed)
```

### player_pairings
```sql
player1_id INTEGER, player2_id INTEGER  -- Canonical: player1_id < player2_id
games_together INTEGER, wins_together INTEGER
games_against INTEGER, player1_wins_against INTEGER
PRIMARY KEY (player1_id, player2_id)
```

## Slash Commands Quick Reference

| Command | Purpose | Key Parameters |
|---------|---------|----------------|
| `/shuffle` | Create balanced teams | `betting_mode`: house/pool |
| `/record` | Record match result | `result`: Radiant/Dire/Abort |
| `/register` | Register player | `steam_id`: Steam32 ID |
| `/setroles` | Set role preferences | `roles`: "1,2,3" or "123" |
| `/stats` | View player stats | `user`: optional target |
| `/lobby` | Create/view lobby | - |
| `/bet` | Place jopacoin bet | `team`, `amount`, `leverage` |
| `/balance` | Check balance/debt | - |
| `/bankruptcy` | Clear debt (1wk cooldown) | - |
| `/leaderboard` | Rankings by jopacoin | `limit`: default 20 |
| `/pairwise` | Teammate/opponent stats | `user`, `min_games` |
| `/profile` | OpenDota profile | `user`: optional |
| `/matchhistory` | Recent matches with stats | `user`, `limit` |
| `/viewmatch` | Detailed match embed | `match_id`, `user` |
| `/dotastats` | Comprehensive stats | `user`: optional |
| `/recent` | Match table as image | `user`, `limit` |
| `/rolesgraph` | Hero role radar graph | `user`, `matches` |
| `/lanegraph` | Lane distribution chart | `user`, `matches` |
| `/hero` | Hero reference lookup | `hero_name` (autocomplete) |
| `/ability` | Ability reference lookup | `ability_name` (autocomplete) |
| `/enrichmatch` | Enrich with Valve data | Admin only |

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

### Test Patterns
```python
# Unit test - mock dependencies
def test_add_player(player_repository):
    player_repository.add(discord_id=123, discord_username="Test", ...)
    assert player_repository.get_by_id(123).name == "Test"

# Integration test - real DB, test service interaction
def test_settle_bets(services):
    match_service = services["match_service"]
    betting_service = services["betting_service"]
    # Full workflow through multiple services

# E2E test - complete user journey
def test_full_match_workflow(test_db, mock_lobby_manager):
    # Register → set roles → join lobby → shuffle → record
```

### Conventions
- Use `repo_db_path` fixture (not `temp_db_path`) for repository tests
- Use `guild_id=None` or `guild_id=0` for single-guild tests
- Mock external APIs (OpenDota, Discord) in integration tests
- Use `time.sleep(0.1)` before cleanup on Windows (file locking)

## Configuration

**Required:** `DISCORD_BOT_TOKEN`

**Optional:**
| Variable | Default | Purpose |
|----------|---------|---------|
| `ADMIN_USER_IDS` | [] | Comma-separated Discord IDs for admin commands |
| `DB_PATH` | cama_shuffle.db | Database file path |
| `OPENDOTA_API_KEY` | None | Higher rate limits (60→1200 req/min) |
| `LOBBY_READY_THRESHOLD` | 10 | Min players to shuffle |
| `LOBBY_MAX_PLAYERS` | 12 | Max players in lobby |
| `OFF_ROLE_MULTIPLIER` | 0.95 | Rating effectiveness off-role |
| `OFF_ROLE_FLAT_PENALTY` | 100.0 | Penalty per off-role player |
| `LEVERAGE_TIERS` | 2,3,5 | Available bet leverage options |
| `MAX_DEBT` | 500 | Maximum negative balance |
| `GARNISHMENT_PERCENTAGE` | 1.0 | Portion of winnings to debt (100%) |
| `BANKRUPTCY_COOLDOWN_SECONDS` | 604800 | 1 week between declarations |

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

## Important Notes

- **Single Instance Lock**: Bot uses `.bot.lock` file to prevent duplicate instances
- **Rating System**: Glicko-2, not simple MMR. Initial RD=350.0, volatility=0.06
- **5 Roles**: 1=Carry, 2=Mid, 3=Offlane, 4=Soft Support, 5=Hard Support (stored as strings)
- **Team Convention**: team1=Radiant, team2=Dire, winning_team: 1 or 2
- **Betting Window**: 15 minutes (BET_LOCK_SECONDS=900) after shuffle
- **Voting Threshold**: 2 non-admin votes OR 1 admin vote to record match
- **Leverage**: Multiplies effective bet; losses can cause debt up to MAX_DEBT
- **Garnishment**: 100% of winnings go to debt repayment until balance >= 0
- **Pairings Storage**: Canonical pairs with player1_id < player2_id to avoid duplicates
- **Lane Outcomes**: W/L/D determined by comparing avg lane_efficiency (parsed matches only)
- **Lane Matchups**: Safe vs Off, Mid vs Mid - 5% threshold for win determination
- **Dotabase**: SQLite database of Dota 2 game data (heroes, abilities, talents, facets)
- **Hero Images**: Steam CDN URLs via `get_hero_image_url()` in `utils/hero_lookup.py`

## Key Dependencies

| Package | Purpose |
|---------|---------|
| `discord.py` | Discord bot framework |
| `glicko2` | Glicko-2 rating calculations |
| `dotabase` | Dota 2 game data (heroes, abilities) |
| `pillow` | Image generation for stats visualization |
| `sqlalchemy-utils` | Required by dotabase |
| `aiohttp` | Async HTTP for OpenDota API |
