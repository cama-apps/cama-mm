# Cama Balanced Shuffle Discord Bot

A Discord bot for balanced team shuffling in Dota 2 inhouse games for the Camaraderous league.

## Features

- **Discord Bot Integration**: Full Discord bot with slash commands
- **Balanced Team Matching**: Minimizes team value difference using Glicko-2 ratings with role-based optimization
- **Captain's Draft Mode**: Coinflip-based captain selection, side/pick choice, and snake draft
- **Dual Rating Systems**: Glicko-2 (primary) and OpenSkill Plackett-Luce (fantasy-weighted)
- **Lobby System**: React-based lobby system for matchmaking
- **Win/Loss Tracking**: Tracks match results for statistics and rating updates
- **Role Distribution**: Role-based balancing with off-role penalties
- **Match Recording**: Record match results with Radiant/Dire team support and voting system
- **Betting System**: Jopacoin wagering on match outcomes with leverage (2x-5x), house/pool modes
- **Prediction Markets**: Create yes/no predictions with community resolution voting
- **Economy Features**: Loans, bankruptcy, Jopacoin Reserve, disbursement voting, tipping, Wheel of Fortune
- **Match Enrichment**: Automatic stats from OpenDota (K/D/A, heroes, GPM, fantasy points)
- **Dota 2 Reference**: Hero and ability lookup with autocomplete
- **Trivia**: Dota 2 trivia with escalating difficulty streaks, covering heroes, items, abilities, facets, and voicelines
- **Stats Visualization**: Image generation for match tables, radar graphs, and charts
- **AI Features** (optional): Narrative/flavor text and natural language queries via the configured Groq or Cerebras LLM
- **SQLite Database**: Lightweight database with automatic migrations

## How It Works

The bot uses a **Glicko-2 rating system** for team balancing. Players are matched to create fair teams that minimize skill difference while optimizing role assignments.

- Teams are balanced using Glicko-2 ratings (or MMR as fallback)
- OpenSkill Plackett-Luce provides an alternative rating weighted by fantasy performance
- Role-based optimization ensures players are matched to their preferred positions
- Off-role penalties encourage proper role distribution
- Win/loss records update player ratings after each match
- Captain's Draft mode allows player-selected teams with coinflip mechanics

## Installation

**Prerequisites:** Python 3.12+ and [uv](https://docs.astral.sh/uv/).

1. **Clone the repository or navigate to the project directory**

2. **Install dependencies:**
   ```bash
   uv sync --frozen
   ```

3. **Set up Discord Bot:**
   - Go to [Discord Developer Portal](https://discord.com/developers/applications)
   - Create a new application
   - Go to "Bot" section and create a bot
   - Copy the bot token
   - Enable the following Privileged Gateway Intents:
     - MESSAGE CONTENT INTENT
     - SERVER MEMBERS INTENT

4. **Configure Bot Permissions:**
   - In the OAuth2 > URL Generator section, select:
     - Scopes: `bot`, `applications.commands`
     - Permissions: View Channels, Send Messages, Manage Messages, Create Public Threads, Send Messages in Threads, Pin Messages, Manage Threads, Embed Links, Read Message History, Use External Emojis, Use External Stickers, Add Reactions

5. **Set up environment variables:**
   - Create a file named `.env` in the project root (same folder as `bot.py`)
   - Add your Discord bot token and admin allowlist (comma-separated Discord user IDs):
     ```
     DISCORD_BOT_TOKEN=your_bot_token_here
     ADMIN_USER_IDS=123456789012345678,234567890123456789
     ```
     If `ADMIN_USER_IDS` is empty, no one is treated as an allowlisted admin for commands like `/addfake`.
   - Optional variables you can include:
     ```
     DB_PATH=/path/to/cama_shuffle.db   # overrides the default sqlite file
     OPENDOTA_API_KEY=your_opendota_key  # unlocks the 1200 req/min rate limit
     DIG_CHANNEL_ID=123456789012345678   # gates /dig commands and routes output to this channel
     ```
     `DB_PATH` defaults to `cama_shuffle.db`; the API and channel settings are optional.

   **Dig channel setup:** when `DIG_CHANNEL_ID` is set, all `/dig *` invocations
   must happen in that channel (or a thread under it) and public dig output
   posts there. To also hide the slash commands from other channels, restrict
   the bot's integration in Discord: Server Settings → Integrations → Cama MM →
   Channels.

6. **Invite bot to your server:**
   - In Discord Developer Portal, go to OAuth2 > URL Generator
   - Select scopes: `bot` and `applications.commands`
   - Select bot permissions as listed in step 4
   - Copy the generated URL and open it in your browser to invite the bot

## Running the Bot

```bash
uv run python bot.py
```

The bot will connect to Discord and sync slash commands automatically.

## Discord Commands

### Registration & Profile

#### `/register`
Register yourself as a player. Fetches your MMR from OpenDota to initialize your Glicko-2 rating.

**Options:**
- `steam_id`: Your **Steam32** ID (found in your Dotabuff URL, e.g., `123456789`)

#### `/linksteam`
Link your Steam account if you're already registered.

**Options:**
- `steam_id`: Your **Steam32** ID

#### `/setroles`
Set your preferred roles for matchmaking.

**Options:**
- `roles`: Roles (1-5, e.g., "123" or "1,2,3" for carry, mid, offlane). Commas and spaces are optional.

#### `/profile`
View comprehensive player profile with tabbed navigation (Overview, Rating, Economy, Gambling, Predictions, Dota, Teammates).

**Options:**
- `user` (optional): Discord user to look up. If omitted, shows your own profile.

### Lobby Management

#### `/lobby`
Create or view the matchmaking lobby. Use buttons in the thread to join/leave. Requires 10+ players to shuffle.

#### `/join`
Join the matchmaking lobby from any channel.

#### `/leave`
Leave the matchmaking lobby.

#### `/kick`
Kick a player from the lobby.

**Options:**
- `player`: The Discord user to kick from the lobby

**Permissions:** Admin or lobby creator only

#### `/resetlobby`
Reset the current lobby (clears all players).

**Permissions:** Admin or lobby creator only

### Match Management

#### `/shuffle`
Create balanced teams from the lobby (requires at least 10 players). Uses pool betting mode with auto-blind liquidity.

#### `/startdraft`
Start a Captain's Draft. Selects captains, runs coinflip, and enables snake draft for team picking.

#### `/setcaptain`
Set yourself or another player as captain for your team during draft.

#### `/record`
Record a match result or abort the match.

**Options:**
- `result`: Choose "Radiant Won", "Dire Won", or "Abort Match"
- `dotabuff_match_id` (optional): Dotabuff match ID for automatic data fetching

### Betting & Economy

#### `/bet`
Place a jopacoin bet on the current match.

**Options:**
- `team`: Choose "Radiant" or "Dire"
- `amount`: Amount of jopacoin to wager
- `leverage` (optional): Multiplier (2x, 3x, or 5x)

#### `/mybets`
Show your active bet for the current match.

#### `/balance`
Check your jopacoin balance and debt status.

#### `/economy tip`
Give jopacoin to another player (1% fee goes to the Jopacoin Reserve).

**Options:**
- `player`: The recipient
- `amount`: Amount to tip

#### `/economy paydebt`
Help another player pay off their debt.

**Options:**
- `player`: The player in debt
- `amount`: Amount to pay

#### `/economy bankruptcy`
Declare bankruptcy to clear debt. Has a 1-week cooldown and 5-game win reward penalty.

#### `/economy loan`
Borrow jopacoin with a 20% fee. Auto-repaid after your next match.

**Options:**
- `amount`: Amount to borrow (max 100)

#### `/economy reserve`
View the Jopacoin Reserve, the server operations budget.

#### `/economy disburse`
Propose or manage Jopacoin Reserve allocation voting.

**Options:**
- `action`: "propose", "status", "reset", "votes", or "execute"

#### `/gamba`
Spin the Wheel of Fortune for random jopacoin outcomes. Daily cooldown.

#### `/shop buy`
Spend jopacoin in the shop for special items.

**Options:**
- `item`: The item to purchase
- `target` (optional): Target player for certain items

#### `/shop pingedash`
Spend jopacoin to send the configured Pingedash. Has a 24-hour cooldown.

#### `/shop avoids`
View your active soft avoids.

#### `/shop deals`
View your active package deals.

#### `/shop mana`
Spend mana on color-exclusive items.

### Predictions

#### `/prediction`
Create a prediction market with yes/no outcomes.

**Options:**
- `question`: The prediction question
- `closes_in`: Time until betting closes (e.g., "1h", "30m")

#### `/predictions`
List all active predictions.

#### `/mypredictions`
View your prediction positions and P&L.

#### `/predictionresolve`
Vote to resolve a prediction. Requires 3 matching votes or 1 admin vote.

**Options:**
- `prediction_id`: The prediction ID
- `outcome`: "yes" or "no"

#### `/predictionclose`
Close prediction betting early.

**Permissions:** Admin only

#### `/predictioncancel`
Cancel a prediction and refund all bets.

**Permissions:** Admin only

### Statistics & Leaderboards

#### `/leaderboard`
View leaderboard with multiple ranking types.

**Options:**
- `type`: "balance", "gambling", "predictions", "glicko", or "openskill"
- `limit` (optional): Number of players to show (default: 20, max: 100)

#### `/calibration`
View rating system health stats and player calibration progress.

**Options:**
- `user` (optional): Discord user to look up

#### `/matchup`
Head-to-head statistics between two players.

**Options:**
- `user1`: First player
- `user2`: Second player

#### `/matches history`
View recent matches with hero picks and stats.

**Options:**
- `user` (optional): Filter by player
- `limit` (optional): Number of matches

#### `/matches view`
Detailed match embed with participant stats.

**Options:**
- `match_id`: The match ID to view

#### `/matches recent`
Recent matches displayed as a formatted image table.

**Options:**
- `user` (optional): Filter by player
- `limit` (optional): Number of matches

### Dota 2 Reference

#### `/hero`
Look up hero information (stats, abilities, talents, facets).

**Options:**
- `hero_name`: Hero name (autocomplete enabled)

#### `/ability`
Look up ability details.

**Options:**
- `ability_name`: Ability name (autocomplete enabled)

### Trivia

#### `/trivia`
Play a Dota 2 trivia question. Questions span heroes, abilities, and items across four difficulty tiers that escalate with your streak:

| Streak | Difficulty | Question types |
|--------|------------|----------------|
| 0–2 | Easy | Hero by image, primary attribute, melee/ranged, ability → hero, ability by icon, item by icon, item cost compare, neutral item tier, damage type |
| 3–5 | Medium | Hero real name, hero by hype, innate ability, voiceline (with portrait) |
| 6–9 | Hard | Scepter/shard upgrades, item cost exact, ability lore, item lore, hero bio, move speed, ability cooldown, ability mana cost, item active cooldown, armor at level 1, attack damage |
| 10+ | Challenging | Voiceline, base attack time, attribute gain per level, night vision, turn rate |

Each question has 4 options with a 30-second timer. A correct answer awards 1 JC and extends your streak; a wrong answer resets it. Streak bonuses add +1 JC at streaks 3 and 6, +2 JC at streak 10, then +1 JC every 4 correct answers after 10.

#### `/trivia-reset-cooldown` (Admin)
Reset the cooldown on the trivia streak for testing or moderation.

### Rating Analysis

#### `/ratinganalysis`
Analyze and compare rating systems with subcommands:
- `compare`: Compare Glicko-2 vs OpenSkill accuracy
- `calibration`: Show calibration curves
- `trend`: Show prediction accuracy over time
- `backfill`: Recalculate OpenSkill from history (Admin)
- `player`: Show player's OpenSkill details

### AI Features (Optional)

#### `/ask`
Ask a question about league data and get an AI-powered answer.

### Help

#### `/help`
List all available commands with descriptions.

### Admin Commands

**Permissions:** Admin only (requires Administrator or Manage Server permission, or Discord ID in `ADMIN_USER_IDS`)

#### `/addfake`
Add fake users to the lobby for testing.

**Options:**
- `count` (optional): Number of fake users to add (1-10, default: 1)

#### `/filllobbytest`
Fill the lobby with test players.

#### `/resetuser`
Reset a specific user's account (wins, losses, rating, jopacoin).

**Options:**
- `user`: Discord user to reset

#### `/registeruser`
Register another user as a player.

**Options:**
- `user`: Discord user to register
- `steam_id`: Their Steam32 ID
- `mmr` (optional): Manual MMR if OpenDota unavailable

#### `/givecoin`
Give or take jopacoin from a user or the Jopacoin Reserve.

**Options:**
- `user`: Target user, or use the reserve option
- `amount`: Amount (negative to take)

#### `/setinitialrating`
Set initial Glicko-2 rating for a player (max 50 games played).

**Options:**
- `user`: Discord user
- `rating`: New rating value

#### `/recalibrate`
Reset a player's rating uncertainty (RD to 350) while keeping their rating. 90-day cooldown, minimum 5 games.

**Options:**
- `user`: Discord user

#### `/extendbetting`
Extend the betting window after shuffle.

**Options:**
- `minutes`: Extension time (1-60)

#### `/setleague`
Set the Dota 2 league ID used by OpenDota for automatic match discovery.

**Options:**
- `league_id`: The Dota 2 league ID

#### `/enrichmatch`
Manually enrich a match with OpenDota data.

#### `/autodiscover`
Auto-discover Dota matches from configured league ID.

#### `/wipematch`
Delete enrichment data for a specific match.

#### `/showconfig`
Display current server configuration.

#### `/rebuildpairings`
Rebuild pairwise teammate/opponent statistics from match history.

#### `/sync`
Force sync slash commands with Discord (useful after command updates).

## Configuration

### Environment Variables

Set these in your `.env` file:

**Required:**
- `DISCORD_BOT_TOKEN` - Your Discord bot token

**Optional:**
| Variable | Default | Description |
|----------|---------|-------------|
| `ADMIN_USER_IDS` | [] | Comma-separated Discord user IDs for admin access |
| `DB_PATH` | cama_shuffle.db | Database file path |
| `OPENDOTA_API_KEY` | None | OpenDota API key for higher rate limits (60→1200 req/min) |

### Advanced Configuration

Additional settings can be configured in `.env` (see `config.py` for all 50+ options):

**Lobby:**
- `LOBBY_READY_THRESHOLD`, `LOBBY_MAX_PLAYERS` - Lobby size settings

**Betting:**
- `LEVERAGE_TIERS` - Available leverage options (default: 2,3,5)
- `MAX_DEBT` - Maximum negative balance (default: 500)
- `BET_LOCK_SECONDS` - Betting window duration (default: 900 / 15 min)
- `AUTO_BLIND_ENABLED`, `AUTO_BLIND_THRESHOLD`, `AUTO_BLIND_PERCENTAGE` - Auto-liquidity settings
- `AUTO_SPECTATOR_BET_ENABLED`, `AUTO_SPECTATOR_BET_COUNT`, `AUTO_SPECTATOR_BET_PERCENTAGE` (default 1% of balance) - Rich spectator auto-wager; spectators may also `/bet` both teams

**Economy:**
- `LOAN_COOLDOWN_SECONDS`, `LOAN_MAX_AMOUNT`, `LOAN_FEE_RATE` - Loan system
- `BANKRUPTCY_COOLDOWN_SECONDS`, `BANKRUPTCY_PENALTY_GAMES` - Bankruptcy settings
- `TIP_FEE_RATE` - Tipping fee (default: 1%)
- `DISBURSE_MIN_FUND`, `DISBURSE_QUORUM_PERCENTAGE` - Disbursement voting

**Wheel of Fortune:**
- `WHEEL_COOLDOWN_SECONDS` - Time between spins (default: 24 hours)
- `WHEEL_TARGET_EV` - Target expected value per spin (default: -10)

**Draft Mode:**
- `PLAYER_STAKE_POOL_SIZE` - Total auto-liquidity for drafts
- `SPECTATOR_POOL_PLAYER_CUT` - Winner share of spectator pool

**Rating:**
- `OFF_ROLE_MULTIPLIER`, `OFF_ROLE_FLAT_PENALTY` - Team balancing penalties
- `RECALIBRATION_COOLDOWN_SECONDS` - Time between rating resets

**AI (Optional):**
- `GROQ_API_KEY`, `CEREBRAS_API_KEY` - Credentials for the configured LLM provider
- `AI_MODEL` - LiteLLM model identifier (`provider/model`)
- `AI_FEATURES_ENABLED` - Default AI setting for guilds without an explicit override (default: False)
- `DIG_LLM_ENABLED` - Process-wide hard kill switch for Dig LLM requests (default: True; restart required)

Without an `AI_MODEL` override, startup selects `groq/qwen/qwen3.6-27b` when a Groq key is present; otherwise it selects the Cerebras fallback, `cerebras/gemma-4-31b`. This is startup selection, not runtime failover: failed Groq requests are not retried on Cerebras. Cerebras access is free-trial and quota-limited.

## Testing

Run the test suite:

```bash
uv run --locked pytest
```

## Troubleshooting

**Bot won't start:** Check `.env` file exists with `DISCORD_BOT_TOKEN` and run `uv sync --frozen`

**Commands not showing:** Wait a few minutes for Discord to sync, or use `/sync` command (admin only)

**Database issues:** Only run one bot instance. Delete `cama_shuffle.db` to reset database if needed.

**Match enrichment failing:** Check OpenDota availability and verify `OPENDOTA_API_KEY` if one is configured.

## License

This project is for the Camaraderous Dota 2 league.
