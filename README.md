# Cama Balanced Shuffle Discord Bot

A Discord bot for balanced team shuffling in Dota 2 inhouse games for the Camaraderous league.

## Features

- **Discord Bot Integration**: Full Discord bot with slash commands
- **Balanced Team Matching**: Minimizes team value difference using Glicko-2 ratings with role-based optimization
- **Lobby System**: React-based lobby system for matchmaking (⚔️ to join/leave)
- **Win/Loss Tracking**: Tracks match results for statistics and Glicko-2 rating updates
- **Role Distribution**: Role-based balancing with off-role penalties
- **Match Recording**: Record match results with Radiant/Dire team support
- **Betting System**: Jopacoin wagering on match outcomes
- **Leaderboard**: Track player rankings by jopacoin balance
- **SQLite Database**: Lightweight database with automatic migrations

## How It Works

The bot uses a **Glicko-2 rating system** for team balancing. Players are matched to create fair teams that minimize skill difference while optimizing role assignments.

- Teams are balanced using Glicko-2 ratings (or MMR as fallback)
- Role-based optimization ensures players are matched to their preferred positions
- Off-role penalties encourage proper role distribution
- Win/loss records update player ratings after each match

## Installation

1. **Clone the repository or navigate to the project directory**

2. **Create virtual environment and install dependencies:**
   ```bash
   uv venv
   uv sync
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
     ```
     Both default to sane values (`cama_shuffle.db` and no API key) if omitted.

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

#### `/setroles`
Set your preferred roles for matchmaking.

**Options:**
- `roles`: Roles (1-5, e.g., "123" or "1,2,3" for carry, mid, offlane). Commas and spaces are optional.

#### `/stats`
View your personal statistics or another player's stats.

**Options:**
- `user` (optional): Discord user to look up. If omitted, shows your own stats.

Displays: Cama Rating (Glicko-2), wins, losses, win rate, jopacoin balance, preferred roles.

### Lobby Management

#### `/lobby`
Create or view the matchmaking lobby. Use buttons in the thread to join/leave. Requires 10+ players to shuffle.

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
Create balanced teams from the lobby (requires at least 10 players).

#### `/record`
Record a match result or abort the match.

**Options:**
- `result`: Choose "Radiant Won", "Dire Won", or "Abort Match"
- `dotabuff_match_id` (optional): Dotabuff match ID for automatic data fetching

### Betting

#### `/bet`
Place a jopacoin bet on the current match.

**Options:**
- `team`: Choose "Radiant" or "Dire"
- `amount`: Amount of jopacoin to wager

#### `/mybets`
Show your active bet for the current match.

#### `/balance`
Check your jopacoin balance.

### Leaderboard

#### `/leaderboard`
View leaderboard sorted by jopacoin balance.

**Options:**
- `limit` (optional): Number of players to show (default: 20, max: 100)

### Help

#### `/help`
List all available commands with descriptions.

### Admin Commands

**Permissions:** Admin only (requires Administrator or Manage Server permission, or Discord ID in `ADMIN_USER_IDS`)

#### `/addfake`
Add fake users to the lobby for testing.

**Options:**
- `count` (optional): Number of fake users to add (1-10, default: 1)

#### `/resetuser`
Reset a specific user's account (wins, losses, rating, jopacoin).

**Options:**
- `user`: Discord user to reset

#### `/sync`
Force sync slash commands with Discord (useful after command updates).

## Configuration

### Environment Variables

Set these in your `.env` file:

**Required:**
- `DISCORD_BOT_TOKEN` - Your Discord bot token

**Optional:**
- `ADMIN_USER_IDS` - Comma-separated Discord user IDs for admin access
- `DB_PATH` - Database file path (default: `cama_shuffle.db`)
- `OPENDOTA_API_KEY` - OpenDota API key for higher rate limits (60 req/min without, 1200 req/min with)

### Advanced Configuration

Additional settings can be configured in `.env` (see `config.py` for all options):
- `LOBBY_READY_THRESHOLD`, `LOBBY_MAX_PLAYERS` - Lobby size settings
- `JOPACOIN_PER_GAME`, `JOPACOIN_MIN_BET`, `JOPACOIN_WIN_REWARD`, `BET_LOCK_SECONDS` - Betting system
- `OFF_ROLE_MULTIPLIER`, `OFF_ROLE_FLAT_PENALTY`, `EXCLUSION_PENALTY_WEIGHT` - Team balancing penalties

## Testing

Run the test suite:

```bash
uv run pytest -n auto
```

## Troubleshooting

**Bot won't start:** Check `.env` file exists with `DISCORD_BOT_TOKEN` and run `uv venv && uv sync`

**Commands not showing:** Wait a few minutes for Discord to sync, or use `/sync` command (admin only)

**Database issues:** Only run one bot instance. Delete `cama_shuffle.db` to reset database if needed.

## License

This project is for the Camaraderous Dota 2 league.
