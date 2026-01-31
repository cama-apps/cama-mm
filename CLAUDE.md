# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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

```
bot.py                    # Entry point, Discord event handlers, service initialization
config.py                 # Environment configuration (all settings centralized here)
database.py               # SQLite wrapper + schema initialization
shuffler.py               # BalancedShuffler - team balancing algorithm
rating_system.py          # CamaRatingSystem - Glicko-2 rating management
openskill_rating_system.py # CamaOpenSkillSystem - OpenSkill Plackett-Luce (fantasy-weighted)
opendota_integration.py   # OpenDotaAPI - rate-limited external API client
steam_api.py              # Valve Web API client + rate limiter
player_queue.py           # In-memory queue helper (not wired into commands yet)
remove_fake_users.py      # CLI script to delete fake users

commands/                 # Discord slash commands (15 cog modules)
├── match.py              # /shuffle, /record
├── registration.py       # /register, /linksteam, /setroles
├── lobby.py              # /lobby, /join, /leave, /kick, /resetlobby
├── betting.py            # /bet, /mybets, /bets, /balance, /tip, /paydebt, /bankruptcy,
│                         # /loan, /nonprofit, /disburse, /gamba
├── info.py               # /help, /leaderboard, /calibration
├── advstats.py           # /matchup, /rebuildpairings
├── enrichment.py         # /setleague, /showconfig, /backfillsteamid, /enrichmatch,
│                         # /matchhistory, /viewmatch, /recent, /autodiscover,
│                         # /wipematch, /wipediscovered
├── dota_info.py          # /hero, /ability (dotabase reference commands)
├── predictions.py        # /prediction, /predictions, /mypredictions, /predictionresolve,
│                         # /predictioncancel, /predictionclose
├── profile.py            # /profile (unified player profile with tabbed navigation)
├── shop.py               # /shop
├── draft.py              # /startdraft, /setcaptain, /restartdraft
├── rating_analysis.py    # /ratinganalysis (compare, calibration, trend, backfill, player)
├── ask.py                # /ask (AI-powered Q&A)
└── admin.py              # /addfake, /filllobbytest, /resetuser, /registeruser, /givecoin,
                          # /resetloancooldown, /resetbankruptcycooldown, /setinitialrating,
                          # /recalibrate, /resetrecalibrationcooldown, /extendbetting,
                          # /correctmatch, /sync

domain/
├── models/               # Pure domain models (no DB dependencies)
│   ├── player.py         # Player dataclass with ratings, roles, balance
│   ├── team.py           # Team with 5 players, role assignments, value calc
│   ├── lobby.py          # Lobby state with regular and conditional players
│   └── draft.py          # DraftState, DraftPhase for captain's draft
└── services/             # Pure domain logic (no side effects)
    ├── role_assignment_service.py   # Optimal role assignment algorithms
    ├── team_balancing_service.py    # Team value and matchup scoring
    └── draft_service.py             # Captain selection, player pool, coinflip

services/                 # Application services (orchestrate repos + domain)
├── match_service.py      # Core: shuffle, record, voting, rating updates (Glicko-2 + OpenSkill)
├── player_service.py     # Registration, role management, stats
├── betting_service.py    # Bet placement, settlement, rewards, auto-blind
├── loan_service.py       # Loans + nonprofit fund tracking
├── disburse_service.py   # Nonprofit disbursement voting + payouts
├── gambling_stats_service.py   # Degen score, gamba stats, leaderboards
├── prediction_service.py # Prediction markets and payouts
├── lobby_service.py      # Lobby embed generation, player formatting
├── lobby_manager_service.py    # Lobby lifecycle with persistence
├── garnishment_service.py      # Debt repayment from winnings
├── bankruptcy_service.py       # Bankruptcy declaration and penalties
├── recalibration_service.py    # Rating RD reset with cooldown
├── match_enrichment_service.py # OpenDota match data enrichment + fantasy points
├── match_discovery_service.py  # Auto-discover Dota match IDs
├── opendota_player_service.py  # Player profile fetching
├── match_state_manager.py      # In-memory pending match state
├── draft_state_manager.py      # In-memory draft state management
├── guild_config_service.py     # Per-guild configuration management
├── rating_comparison_service.py # Glicko-2 vs OpenSkill analysis
├── ai_service.py               # LiteLLM/Cerebras integration
├── flavor_text_service.py      # AI-generated flavor text for events
├── sql_query_service.py        # Natural language to SQL queries
└── permissions.py              # Admin permission checking

repositories/             # Data access layer
├── interfaces.py         # Abstract interfaces (IPlayerRepository, etc.)
├── base_repository.py    # Connection management, context managers
├── player_repository.py  # Player CRUD, balance, ratings, steam_id, OpenSkill
├── match_repository.py   # Match recording, enrichment, participants, fantasy
├── bet_repository.py     # Bet placement, settlement (atomic operations)
├── disburse_repository.py    # Nonprofit disbursement proposals/votes/history
├── lobby_repository.py   # Lobby state persistence (regular + conditional players)
├── pairings_repository.py    # Pairwise teammate/opponent stats
├── prediction_repository.py   # Prediction markets data access
├── guild_config_repository.py    # Per-guild configuration
├── recalibration_repository.py   # Recalibration state tracking
├── tip_repository.py         # Tip transaction history
└── ai_query_repository.py    # AI query caching

infrastructure/
└── schema_manager.py     # SQLite schema creation and 49 migrations

utils/
├── embeds.py             # Discord embed builders (lobby, match, enriched stats)
├── formatting.py         # Role emojis, betting display, pool odds, constants
├── rate_limiter.py       # Token-bucket rate limiting for commands
├── hero_lookup.py        # Hero ID → name, image URL (via heroes.json)
├── drawing.py            # Image generation (Pillow): match tables, radar graphs, bar charts
├── wheel_drawing.py      # Wheel of Fortune GIF animation for /gamba
├── draft_embeds.py       # Draft mode embed formatting
├── rating_insights.py    # Rating system analytics and calibration stats
├── role_assignment_cache.py  # LRU cache for role assignment optimization
├── interaction_safety.py # Safe defer/followup for Discord interactions
└── debug_logging.py      # JSONL debug tracing (optional)

docs/                     # Deep-dive docs (server setup, stats, ratings insights)

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
- `tip_atomic()` - atomic tipping with fees

### Service Dependencies
```
MatchService
├── IPlayerRepository
├── IMatchRepository
├── TeamBalancingService → RoleAssignmentService
├── CamaRatingSystem (Glicko-2)
├── CamaOpenSkillSystem (OpenSkill Plackett-Luce)
├── BalancedShuffler
├── BettingService (optional, unified for shuffle + draft)
│   ├── BetRepository
│   ├── PlayerRepository
│   ├── GarnishmentService
│   └── BankruptcyService
├── LoanService (optional)
└── IPairingsRepository (optional)
```

## Domain Models

### Player (`domain/models/player.py`)
```python
@dataclass
class Player:
    name: str
    mmr: int | None              # OpenDota MMR (0-12000)
    initial_mmr: int | None      # Starting MMR
    wins: int = 0
    losses: int = 0
    preferred_roles: list[str]   # ["1", "2", "3", "4", "5"]
    main_role: str | None
    glicko_rating: float | None  # Cama rating (0-3000)
    glicko_rd: float | None      # Rating deviation (uncertainty)
    glicko_volatility: float | None
    os_mu: float | None          # OpenSkill mean (fantasy-weighted)
    os_sigma: float | None       # OpenSkill sigma
    discord_id: int | None
    jopacoin_balance: int = 0

    def get_value(use_glicko=True) -> float  # For team balancing
    def has_role(role: str) -> bool          # Check role preference
    def get_win_rate() -> float | None       # Win percentage
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
    def get_player_by_role(role) -> tuple[Player, float]
```

### Lobby (`domain/models/lobby.py`)
```python
class Lobby:
    lobby_id: int
    players: set[int]              # Regular queue Discord IDs
    conditional_players: set[int]  # "Frogling" conditional players
    status: str                    # "open" or "closed"

    def is_ready(min_players=10) -> bool
    def add_player(discord_id) -> bool
    def add_conditional_player(discord_id) -> bool
    def get_total_count() -> int
```

### DraftState (`domain/models/draft.py`)
```python
class DraftPhase(Enum):
    COINFLIP, WINNER_CHOICE, WINNER_SIDE_CHOICE, WINNER_HERO_CHOICE,
    LOSER_CHOICE, PLAYER_DRAFT_ORDER, DRAFTING, COMPLETE

class DraftState:
    guild_id: int
    player_pool_ids: list[int]       # 10 selected players
    excluded_player_ids: list[int]   # Excluded from shuffle
    captain1_id, captain2_id: int | None
    radiant_captain_id, dire_captain_id: int | None
    coinflip_winner_id: int | None
    winner_choice_type: str | None   # "side" or "hero_pick"
    radiant_player_ids, dire_player_ids: list[int]
    phase: DraftPhase

    def pick_player(player_id) -> bool
    def available_player_ids -> list[int]
    def is_draft_complete -> bool
```

## Key Services

### MatchService (`services/match_service.py`)
Core orchestrator for matches. Thread-safe via `_recording_lock`.

```python
# Shuffle players into balanced teams
shuffle_players(player_ids, guild_id, betting_mode) -> dict

# Record match result (handles voting, ratings, bets)
record_match(winning_team, guild_id, dotabuff_match_id) -> dict

# Correct an incorrectly recorded match result
correct_match_result(match_id, new_winning_team, guild_id, corrected_by) -> dict

# OpenSkill rating updates
update_openskill_ratings_for_match(match_id, radiant_won, fantasy_weights) -> dict
backfill_openskill_ratings() -> dict
```

### BettingService (`services/betting_service.py`)
Handles jopacoin wagering with two modes:
- **House mode**: 1:1 fixed odds
- **Pool mode**: Parimutuel (odds from bet distribution) with auto-blind

```python
place_bet(guild_id, discord_id, team, amount, pending_state, leverage) -> None
settle_bets(match_id, guild_id, winning_team, pending_state) -> dict
create_auto_blind_bets(pending_state, guild_id) -> list  # Auto-liquidity
award_participation(player_ids) -> dict  # 1 jopacoin per game
award_win_bonus(winning_ids) -> dict     # JOPACOIN_WIN_REWARD per win
```

### LoanService (`services/loan_service.py`)
Handles loans, cooldowns, and nonprofit fund accounting.

```python
can_take_loan(discord_id, amount) -> dict
take_loan(discord_id, amount, guild_id=None) -> dict
repay_loan(discord_id, guild_id=None) -> dict
get_nonprofit_fund(guild_id) -> int
```

### PredictionService (`services/prediction_service.py`)
Prediction market lifecycle, voting, and settlement.

```python
create_prediction(guild_id, creator_id, question, closes_at) -> dict
place_bet(prediction_id, discord_id, position, amount) -> dict
add_resolution_vote(prediction_id, user_id, outcome) -> dict
resolve_prediction(prediction_id, outcome, resolved_by) -> dict
```

### AIService (`services/ai_service.py`)
Optional LLM integration via Cerebras.

```python
call_model(prompt, system_prompt=None) -> str
generate_flavor_text(event, context) -> str
execute_sql_query(question) -> dict
```

## Database Schema (Key Tables)

### players
```sql
discord_id INTEGER PRIMARY KEY
discord_username TEXT NOT NULL
glicko_rating REAL, glicko_rd REAL, glicko_volatility REAL
os_mu REAL, os_sigma REAL  -- OpenSkill Plackett-Luce
preferred_roles TEXT  -- JSON array ["1", "2"]
jopacoin_balance INTEGER DEFAULT 3
exclusion_count INTEGER DEFAULT 0
steam_id INTEGER UNIQUE
last_wheel_spin INTEGER  -- Unix timestamp for /gamba cooldown
lowest_balance_ever INTEGER  -- For degen scoring
```

### matches
```sql
match_id INTEGER PRIMARY KEY AUTOINCREMENT
team1_players TEXT, team2_players TEXT  -- JSON arrays (Radiant/Dire)
winning_team INTEGER  -- 1=Radiant, 2=Dire
lobby_type TEXT  -- 'shuffle' or 'draft'
valve_match_id INTEGER  -- For enrichment
duration_seconds INTEGER, radiant_score INTEGER, dire_score INTEGER
enrichment_data TEXT  -- JSON blob for detailed stats
```

### match_participants
```sql
match_id INTEGER, discord_id INTEGER  -- Composite PK
team_number INTEGER, side TEXT, won INTEGER
hero_id INTEGER, kills INTEGER, deaths INTEGER, assists INTEGER
gpm INTEGER, xpm INTEGER, net_worth INTEGER
lane_role INTEGER, lane_efficiency INTEGER  -- Laning phase
fantasy_points REAL  -- Calculated fantasy score
```

### bets
```sql
guild_id INTEGER NOT NULL DEFAULT 0
discord_id INTEGER NOT NULL
team_bet_on TEXT  -- 'radiant' or 'dire'
amount INTEGER, leverage INTEGER DEFAULT 1
is_blind INTEGER DEFAULT 0  -- Auto-blind flag
odds_at_placement REAL  -- Historical odds
payout INTEGER  -- NULL for pending/lost
```

### wheel_spins
```sql
spin_id INTEGER PRIMARY KEY AUTOINCREMENT
guild_id INTEGER, discord_id INTEGER
result TEXT  -- WIN/LOSE/BANKRUPT/value
spin_time INTEGER
```

### predictions
```sql
prediction_id INTEGER PRIMARY KEY AUTOINCREMENT
guild_id INTEGER, question TEXT
status TEXT  -- open/closed/resolved
outcome TEXT, closes_at INTEGER
resolution_votes TEXT  -- JSON {discord_id: outcome}
```

## Slash Commands Quick Reference

| Command | Purpose | Key Parameters |
|---------|---------|----------------|
| `/help` | List all available commands | - |
| `/lobby` | Create/view lobby | - |
| `/join` | Join the matchmaking lobby | - |
| `/leave` | Leave the matchmaking lobby | - |
| `/kick` | Remove a user from lobby | `user` |
| `/resetlobby` | Reset lobby state | Admin only |
| `/shuffle` | Create balanced teams (pool betting) | - |
| `/record` | Record match result | `result`: Radiant/Dire/Abort |
| `/startdraft` | Start captain's draft | - |
| `/setcaptain` | Set your team's captain | - |
| `/restartdraft` | Restart current draft | Admin only |
| `/register` | Register player | `steam_id`: Steam32 ID |
| `/linksteam` | Link Steam account if registered | `steam_id`: Steam32 ID |
| `/setroles` | Set role preferences | `roles`: "1,2,3" or "123" |
| `/profile` | Unified player profile (7 tabs) | `user`: optional |
| `/calibration` | Rating system stats | `user`: optional |
| `/leaderboard` | Rankings | `type`: balance/gambling/predictions/glicko/openskill |
| `/bet` | Place jopacoin bet | `team`, `amount`, `leverage` |
| `/mybets` | Show active bets | - |
| `/bets` | Show all pool bets | Admin only |
| `/balance` | Check balance/debt | - |
| `/tip` | Give jopacoin to player | `player`, `amount` |
| `/paydebt` | Help pay another's debt | `user`, `amount` |
| `/bankruptcy` | Clear debt (1wk cooldown) | - |
| `/loan` | Borrow jopacoin | `amount` |
| `/nonprofit` | View nonprofit fund | - |
| `/disburse` | Manage fund distribution | `action`: propose/status/reset |
| `/gamba` | Spin Wheel of Fortune | Daily cooldown |
| `/shop` | Spend jopacoin | `item`, `target` |
| `/prediction` | Create prediction market | `question`, `closes_in` |
| `/predictions` | List active predictions | - |
| `/mypredictions` | View your positions | - |
| `/predictionresolve` | Vote to resolve | `prediction_id`, `outcome` |
| `/predictionclose` | Close betting early | Admin only |
| `/predictioncancel` | Cancel a prediction | Admin only |
| `/matchup` | Head-to-head stats | `user1`, `user2` |
| `/rebuildpairings` | Rebuild pairings table | Admin only |
| `/setleague` | Set Valve league ID | `league_id` |
| `/showconfig` | Show server config | Admin only |
| `/enrichmatch` | Enrich with Valve data | Admin only |
| `/autodiscover` | Auto-discover matches | Admin only |
| `/wipematch` | Delete match enrichment | Admin only |
| `/matchhistory` | Recent matches | `user`, `limit` |
| `/viewmatch` | Detailed match embed | `match_id` |
| `/recent` | Match table as image | `user`, `limit` |
| `/hero` | Hero reference | `hero_name` (autocomplete) |
| `/ability` | Ability reference | `ability_name` (autocomplete) |
| `/ratinganalysis` | Rating system analysis | Subcommands: compare/calibration/trend/backfill/player |
| `/ask` | AI-powered Q&A | `question` (modal) |
| `/addfake` | Add fake users | `count` |
| `/filllobbytest` | Fill lobby with test players | Admin only |
| `/resetuser` | Reset user account | `user` |
| `/registeruser` | Register another user | `user`, `steam_id` |
| `/givecoin` | Give/take jopacoin | `user`, `amount` |
| `/setinitialrating` | Set initial rating | `user`, `rating` |
| `/recalibrate` | Reset rating uncertainty | `user` (Admin) |
| `/extendbetting` | Extend betting window | `minutes`: 1-60 |
| `/correctmatch` | Correct recorded match result | `match_id`, `correct_result` |
| `/sync` | Force sync commands | Admin only |

**Consolidated Commands:**
- `/profile` replaces: `/stats`, `/gambastats`, `/gambachart`, `/predictionstats`, `/dotastats`, `/rolesgraph`, `/lanegraph`, `/pairwise`
- `/leaderboard type:gambling` replaces: `/gambaleaderboard`
- `/leaderboard type:predictions` replaces: `/predictionleaderboard`

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

**Required:** `DISCORD_BOT_TOKEN`

**Optional:**
| Variable | Default | Purpose |
|----------|---------|---------|
| `ADMIN_USER_IDS` | [] | Comma-separated Discord IDs for admin commands |
| `DB_PATH` | cama_shuffle.db | Database file path |
| `OPENDOTA_API_KEY` | None | Higher rate limits (60→1200 req/min) |
| `STEAM_API_KEY` | None | Valve API key for match history/enrichment |
| `DEBUG_LOG_PATH` | None | Enable JSONL debug logging when set |
| `LOBBY_READY_THRESHOLD` | 10 | Min players to shuffle |
| `LOBBY_MAX_PLAYERS` | 14 | Max players in lobby |
| `OFF_ROLE_MULTIPLIER` | 0.95 | Rating effectiveness off-role |
| `OFF_ROLE_FLAT_PENALTY` | 350.0 | Penalty per off-role player |
| `LEVERAGE_TIERS` | 2,3,5 | Available bet leverage options |
| `MAX_DEBT` | 500 | Maximum negative balance |
| `GARNISHMENT_PERCENTAGE` | 1.0 | Portion of winnings to debt (100%) |
| `BANKRUPTCY_COOLDOWN_SECONDS` | 604800 | 1 week between declarations |
| `BANKRUPTCY_PENALTY_GAMES` | 5 | Win reward penalty games |
| `LOAN_COOLDOWN_SECONDS` | 259200 | 3 days between loans |
| `LOAN_MAX_AMOUNT` | 100 | Max loan size |
| `LOAN_FEE_RATE` | 0.20 | Loan fee rate (20%) |
| `DISBURSE_MIN_FUND` | 250 | Min nonprofit fund to propose |
| `DISBURSE_QUORUM_PERCENTAGE` | 0.40 | Vote quorum (40%) |
| `TIP_FEE_RATE` | 0.01 | Tipping fee rate (1%) |
| `WHEEL_COOLDOWN_SECONDS` | 86400 | 24 hours between /gamba spins |
| `WHEEL_TARGET_EV` | -10.0 | Target expected value per spin |
| `AUTO_BLIND_ENABLED` | True | Auto-blind in pool mode (shuffle + draft) |
| `AUTO_BLIND_THRESHOLD` | 50 | Min balance for auto-blind |
| `AUTO_BLIND_PERCENTAGE` | 0.05 | Bet size as % of balance |
| `CEREBRAS_API_KEY` | None | AI service API key |
| `AI_FEATURES_ENABLED` | False | Global AI toggle |
| `RECALIBRATION_COOLDOWN_SECONDS` | 7776000 | 90 days between recalibrations |

See `config.py` for the full list (50+ options).

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

- **Dual Rating Systems**: Glicko-2 (primary, probabilistic) and OpenSkill Plackett-Luce (fantasy-weighted alternative)
- **5 Roles**: 1=Carry, 2=Mid, 3=Offlane, 4=Soft Support, 5=Hard Support (stored as strings)
- **Team Convention**: team1=Radiant, team2=Dire, winning_team: 1 or 2
- **Match Types**: `lobby_type` = "shuffle" (random balanced) or "draft" (captain's pick)
- **Betting Window**: 15 minutes (BET_LOCK_SECONDS=900) after shuffle; admins can extend via `/extendbetting`
- **Voting Threshold**: 2 non-admin votes OR 1 admin vote to record match
- **Leverage**: Multiplies effective bet; losses can cause debt up to MAX_DEBT
- **Garnishment**: 100% of winnings go to debt repayment until balance >= 0
- **Auto-Blind**: Pool mode auto-generates blind bets for liquidity (5% of balance for players with 50+ JC) - works for both shuffle and draft
- **Unified Betting**: Both shuffle and draft modes use the same BettingService with full leverage, multi-bet, and debt support
- **Loans**: One outstanding loan at a time; repayment runs on match record; fees fund nonprofit
- **Disbursement**: Requires quorum; methods are even/proportional/neediest/stimulus
- **Predictions**: Resolution threshold is 3 matching votes or 1 admin vote
- **Wheel of Fortune**: Daily spin with WIN/LOSE/BANKRUPT outcomes; target EV of -10 JC
- **Degen Score**: 0-100 based on leverage addiction (40%), bet frequency (20%), bankruptcies (20%), loss chasing (10%), paper hands (10%)
- **Fantasy Points**: Calculated from OpenDota stats (kills, deaths, gpm, towers, runes, etc.)
- **Conditional Players**: "Froglings" who only play if needed to reach 10 players. In both `/shuffle` and `/startdraft`, regular players are always included first; conditional players are randomly selected (not rating-based) to fill remaining spots up to 10. If there are ≥10 regular players, conditional players are excluded entirely.
- **Recalibration**: Admins can reset a player's RD to 350 (90-day cooldown, min 5 games)
- **Pairings Storage**: Canonical pairs with player1_id < player2_id to avoid duplicates
- **Schema**: 50 migrations total

## Key Dependencies

| Package | Purpose |
|---------|---------|
| `discord.py` | Discord bot framework |
| `glicko2` | Glicko-2 rating calculations |
| `openskill` | OpenSkill Plackett-Luce ratings |
| `dotabase` | Dota 2 game data (heroes, abilities) |
| `pillow` | Image generation for stats visualization |
| `aiohttp` | Async HTTP for OpenDota API |
| `litellm` | LLM abstraction for AI features |
