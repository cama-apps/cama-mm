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
- **Betting System**: Jopacoin wagering on match outcomes with leverage (2x-10x), house/pool modes
- **Prediction Markets**: Create yes/no predictions with community resolution voting
- **Economy Features**: Loans, bankruptcy, Jopacoin Reserve, disbursement voting, tipping, Wheel of Fortune
- **Tax Man**: Guild monetary policy — audits, fines, economy events, central ledger, and a vanity tax on profits of members without a server nickname
- **Dig Minigame**: Tunnel-digging game with gear, artifacts, weather, insurance, sabotage, prestige, and miner builds
- **Pets**: Adopt and raise a cama (camel-llama hybrid) — feeding, trinkets, brawls, a memorial graveyard, and a sacrificial altar
- **Daily Mafia**: Social-deduction subgame with secret roles, night actions, and day-phase lynch votes
- **Duels**: Challenges of honor between players
- **Mana**: Daily mana land assignments that feed color-exclusive shop items
- **Match Enrichment**: Automatic stats from OpenDota (K/D/A, heroes, GPM, fantasy points)
- **Dota 2 Reference**: Hero and ability lookup with autocomplete
- **Trivia**: Dota 2 trivia with escalating difficulty streaks, plus a daily player-stats trivia set
- **Cama Wrapped**: Personal year-in-review stats
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

3. **Configure Discord:**
   - In the [Developer Portal](https://discord.com/developers/applications), create the bot and enable the Message Content, Server Members, and Presence intents. Server Members supplies nickname updates; Manage Nicknames is not required.
   - Under OAuth2 > URL Generator, select the `bot` and `applications.commands` scopes, then grant: View Channels, Send Messages, Manage Messages, Create Public Threads, Send Messages in Threads, Pin Messages, Manage Threads, Embed Links, Read Message History, Use External Emojis, Use External Stickers, and Add Reactions.
   - Open the generated URL to invite the bot.

4. **Set up environment variables:**
   - Create a file named `.env` in the project root (same folder as `bot.py`)
   - Add your Discord bot token and admin allowlist (comma-separated Discord user IDs):
     ```
     DISCORD_BOT_TOKEN=your_bot_token_here
     ADMIN_USER_IDS=123456789012345678,234567890123456789
     ```
     If `ADMIN_USER_IDS` is empty, no one is treated as an allowlisted admin for commands like `/admin addfake`.
   - Optional variables you can include:
     ```
     DB_PATH=/path/to/cama_shuffle.db   # overrides the default sqlite file
     OPENDOTA_API_KEY=your_opendota_key  # unlocks the 1200 req/min rate limit
     DIG_CHANNEL_ID=123456789012345678   # gates /dig commands and routes output to this channel
     LOBBY_CHANNEL_ID=123456789012345678 # regular lobby embeds post here instead of the command channel
     LOWSKILL_LOBBY_CHANNEL_ID=234567890123456789 # optional Whine & Cheese channel; defaults to LOBBY_CHANNEL_ID
     PET_CHANNEL_ID=123456789012345678   # REQUIRED for pets: without it the /pet cog is not loaded
     ```
     `DB_PATH` defaults to `cama_shuffle.db`; the API and channel settings are optional.

   **Dig channel setup:** when `DIG_CHANNEL_ID` is set, all `/dig *` invocations
   must happen in that channel (or a thread under it) and public dig output
   posts there. To also hide the slash commands from other channels, restrict
   the bot's integration in Discord: Server Settings → Integrations → Cama MM →
   Channels.

   **Pet channel setup:** the entire pets feature is gated on `PET_CHANNEL_ID`.
   Until it is set, the `/pet` cog is not loaded, no pet service is created, and
   the match/profile pet hooks no-op. When set, hatch announcements, obituaries,
   and weekly refund summaries post to that channel.

## Running the Bot

```bash
uv run python bot.py
```

The bot will connect to Discord and sync slash commands automatically.

## Rust lift-and-shift

The replacement implementation lives in [`rust/`](rust/README.md). Python
remains the production runtime only while the migration is incomplete; the
terminal deployment stops the Python container and runs `cama-rust` as the sole
Discord gateway, SQLite migration authority, worker host, and database writer.
The Rust workspace has an exact test-parity ledger, production gateway and
migration foundations, CI gates, and a mechanical inventory that prevents a
partially wired bot from being called cutover-ready. The linked guide defines
the shared process lock, safe database-snapshot flow, Python-off rehearsal,
rollback criteria, and current measured status. Deployment now has one
fail-closed selector, `BOT_RUNTIME=python|rust`; unset still means Python, and
the Rust choice remains blocked until every cutover-readiness gate is complete.

## Discord Commands

The bot registers **45 top-level commands/groups totaling 190 command paths**. The
core flows (lobby/shuffle, draft, match recording, betting) are documented in
full below; everything else gets a summary — run `/help` in Discord for the
complete list, and Discord's slash-command autocomplete shows per-option details.

### Lobby & Shuffle

There are two lobbies — 🍽️ All You Can Feed (open to all ratings) and 🧀 Whine & Cheese
(Glicko below 1400) — and you can queue in **both at once** if you're eligible. Whichever one
shuffles or drafts first takes you; you're removed from the other automatically, and the
players it benched keep their place in it.

Because of that, any command that would otherwise read "the lobby you're in" takes a `lobby`
option. It's inferred when you're only in one and required when you're in both.

#### `/lobby`
Create or view a matchmaking lobby. Use buttons in the thread to join/leave. Requires 10+ players to shuffle.

**Options:**
- `lobby` (optional): Which lobby to create or view (defaults to All You Can Feed)

#### `/join`
Join a matchmaking lobby from any channel.

**Options:**
- `lobby` (optional): Which lobby to join (defaults to All You Can Feed)

#### `/leave`
Leave every matchmaking lobby you're queued in. A lobby whose shuffle or draft has already
started keeps you until it finishes.

#### `/readycheck`
Check lobby players' online status and ping those who are away.

**Options:**
- `lobby` (optional): Which lobby to check (defaults to the lobby you're in)

#### `/kick`
Kick a player from the lobbies they're in, once. This does not prevent them from rejoining.

**Options:**
- `player`: The Discord user to kick from the lobby
- `reason` (optional): Private reason shown to the player and retained in admin history

**Permissions:** Admin — reaches both lobbies. A lobby creator reaches only the lobby they
created, while they are still in it.

#### `/resetlobby`
Reset a lobby (clears all players).

**Options:**
- `lobby` (optional): Which lobby to reset (defaults to the lobby you're in)

**Permissions:** Admin or lobby creator only

#### `/shuffle`
Create balanced teams from the lobby (requires at least 10 players). Uses pool betting mode with auto-blind liquidity.

**Options:**
- `mode` (optional): "Balanced" (default) or "Region Split" (US West vs US East)
- `rating_system` (optional): "Glicko-2" (default), "OpenSkill" (experimental), or "Jopacoin Balance"
- `lobby` (optional): Which lobby to shuffle (defaults to the lobby you're in)

### Captain's Draft — `/draft`

#### `/draft start`
Start an Immortal Draft with captain-based player selection: coinflip, side/pick choice, and snake draft.

**Options:**
- `captain1` (optional): Specify first captain
- `captain2` (optional): Specify second captain
- `lobby` (optional): Which lobby to draft from (defaults to the lobby you're in)

Locking the draft pool counts as the lobby popping: those players are removed from the other
lobby when the draft starts, not when it finishes.

#### `/draft restart`
Restart the current Immortal Draft, preserving the lobby. Captains or admins only.

#### `/draft samplecomplete` / `/draft sampleinprogress`
Admin-only sample draft UI renders for testing.

### Match Recording

#### `/record`
Record a match result or abort the match.

**Options:**
- `result`: Choose "Radiant Won", "Dire Won", or "Abort Match"
- `dotabuff_match_id` (optional): Dotabuff match ID for automatic data fetching

### Betting

#### `/bet`
Place a jopacoin bet on a match.

**Options:**
- `team`: Choose "Radiant" or "Dire"
- `amount`: Amount of jopacoin to wager
- `leverage` (optional): Multiplier (2x, 3x, 5x, or 10x) — can cause debt!
- `match` (optional): Match to bet on; auto-selects if you're a participant or only one match exists

#### `/mybets`
Show your active bets.

#### `/bets`
Show all bets in the current pool (optional `match` filter).

#### `/gamba`
Spin the Wheel of Fortune for random jopacoin outcomes. Daily cooldown.

### Economy — `/economy`

- `/economy tip` — Give jopacoin to another player (1% fee goes to the Jopacoin Reserve)
- `/economy invest` — Configure free shuffle-time long/short wagers (`set`, `remove`, `list`), capped at 10% per target and 50% total; activates at 50+ jopacoin
- `/economy paydebt` — Help another player pay off their debt
- `/economy bankruptcy` — Declare bankruptcy to clear debt (1-week cooldown; win-reward penalty for your next 3 wins)
- `/economy loan` — Borrow up to 100 jopacoin with a 20% fee, auto-repaid after your next match
- `/economy reserve` — View the Jopacoin Reserve, the server operations budget
- `/economy disburse` — Propose or manage Reserve allocation voting (`propose`, `status`, `reset`, `votes`, `execute`)

### Shop — `/shop`

- `/shop buy` — Spend jopacoin on special items (some take a `target` player)
- `/shop pingedash` / `/shop pingedkevin` — Paid pings of the configured targets, each on an independent 24-hour cooldown
- `/shop avoids` — View your active soft avoids
- `/shop deals` — View your active package deals
- `/shop cancel-deal target:@player` — Cancel a package deal after both the target and deal have been inactive for 30 days (no refund)
- `/shop mana` — Spend mana on color-exclusive items

### Predictions — `/predict`

- `/predict create` — Create a prediction market (admin)
- `/predict list` / `/predict view` / `/predict mine` — Browse markets, market detail (price, ladder, trades), and your positions
- `/predict resolve` / `/predict cancel` — Resolve YES/NO or cancel and refund (admin)
- `/predict help` plus admin maintenance subcommands (`set_fair`, `rollback`, `refresh_status`, `force_refresh`)

### Registration & Profile — `/player`

- `/player register` — Register yourself with your Steam32 ID; fetches MMR from OpenDota to seed your rating
- `/refer` — Refer a player before their first game
- `/player link` / `/player unlink` / `/player steamids` — Link, unlink, and list your Steam accounts
- `/player roles` — Set preferred roles (1-5) for matchmaking
- `/player region` — Set your preferred Dota server (US East / US West)
- `/player exclusion` — Check your exclusion factor
- `/player lobby autonotify enabled:<true|false>` — Enable or disable persistent public alerts when lobbies reach 8 and 9 players
- `/player lobby autonotify playername:<player>` — Get one DM the next time that player joins a lobby in this server

#### `/profile`
View comprehensive player profile with tabbed navigation (Overview, Rating, Economy, Gambling, Predictions, Dota, Teammates). Optional `user` to look up someone else.

### Statistics & Leaderboards

#### `/leaderboard`
View leaderboard with multiple ranking types.

**Options:**
- `type`: "Balance" (default), "Glicko-2 Rating", "OpenSkill Rating", "Gambling", "Tips", or "Trivia"
- `limit` (optional): Number of entries to show (default: 100, max: 100)

- `/calibration` — Rating system health stats and player calibration progress
- `/matchup` — Head-to-head statistics between two players
- `/matches history` / `/matches view` / `/matches recent` — Recent matches, detailed match embed, and image-table view
- `/ratinganalysis` — Compare rating systems (`compare`, `calibration`, `trend`, `backfill`, `player`) (Admin)
- `/wrapped` — Your Cama Wrapped year in review
- `/herogrid` — Player x hero grid image showing hero pools and win rates
- `/scout report` / `/scout links` — Hero scouting report and Dotabuff links for players in the current game

### Minigames & Extras

- `/dig` — Tunnel digging minigame: 25 player subcommands (`go`, `gear`, `shop`, `buy`, `use`, `inventory`, `artifacts`, `insure`, `trap`, `sabotage`, `gift`, `flex`, `prestige`, `weather`, `abandon`, `leaderboard`, `halloffame`, `guide`, `help`, `info`, and the `/dig miner` build system: `about`, `autobuy`, `build`, `profile`, `respec`) plus `/dig admin resetcooldown|forceevent|setdepth`. Gated to the configured dig channel.
- `/pet` — Cama pet care: `adopt`, `status`, `feed`, `shop`, `buy`, `rename`, `trinket`, `brawl` (challenge someone to a pet brawl), `altar` (sacrifice your cama for a better egg), `graveyard`, `leaderboard`. Requires `PET_CHANNEL_ID`.
- `/mafia` — Daily Mafia: `join`, `role`, `act`, `vote`, `remind`, `status`, `bounty`, `history`, `leaderboard`, `info`, `optin`, `optout`, plus `/mafia admin start|stop|abort`. Runs in the dedicated mafia channel.
- `/duel` — Challenges of honor: `issue`, `respond`, `list`, `resolve`. Pending,
  unresolved, and expiry announcements prefer the guild's unique `#dota-mm`
  text channel and fall back to the originating channel. The original challenge
  message is updated in place.
- `/tax` — Jopacoin economy and Tax Man tools: `audit`, `policy`, `player`, `ledger`, `event`, `fine`, `vanity` (check the nickname vanity tax), `bankruptcy`, `resetcooldown`
- `/mana` — Check your daily mana land assignment
- `/trivia` — Dota 2 trivia with four difficulty tiers that escalate with your streak (heroes, items, abilities, facets, voicelines). Four options per question with a 15-second timer; a correct answer awards 1 JC plus streak bonuses, a wrong answer resets the streak. 6-hour cooldown.
- `/playertrivia` — Daily trivia set about this server's player stats
- `/setreminder` — Configure DM reminders for cooldowns and match betting windows

### Surveys — `/survey`

- Admins can create, list, preview, edit, delete, send, retry, close, and view results for one-off surveys delivered by DM.
- `/survey question add|edit|remove|move` configures up to 10 ordered NPS (0–10), rating (1–5), or free-text questions while the survey is a draft.
- `/survey send` targets registered players, one server member, or a role; member and role sends can be limited to registered players.
- Responses are submitted from a private review screen. Results show response rates and anonymous per-question aggregates or text responses without respondent identities or cross-question correlation.

### Dota 2 Reference — `/dota`

- `/dota hero` — Hero information (stats, abilities, talents, facets) with autocomplete
- `/dota ability` — Ability details with autocomplete

### AI Features (Optional)

#### `/ask`
Ask a question about league data and get an AI-powered answer.

### Help

#### `/help`
List all available commands with descriptions.

### Admin

**Permissions:** Admin only (requires Administrator or Manage Server permission, or Discord ID in `ADMIN_USER_IDS`)

- `/admin` — Maintenance subcommands: `addfake`, `filllobbytest`, `resetuser`, `registeruser`, `givecoin`, `setrating`, `bumprd`, `adjust rating|rd`, `recalibrate`, `extendbetting`, `correctmatch`, `sync`, `health`, `seedherogrid`, Steam ID management (`addsteamid`, `removesteamid`, `setprimarysteam`), and cooldown resets (`resetbankruptcycooldown`, `resetloancooldown`, `resetrecalibrationcooldown`)
- `/admin moderation suspend|lift|status|list|history` — Temporarily block selected lobby kinds by duration, future completed matches, or both, with private reasons and durable history
- `/admin lowprio add|remove|status|list` — Admin-only private management and inspection of low-priority shuffle modifiers and 1.1× positive rating gains for 1–20 required wins
- `/player lobby status` — Privately view your active lobby suspension
- `/enrich` — Match enrichment and discovery: `setleague`, `discover`, `match`, `backfill`, `wipematch`, `wipeall`, `rebuildpairings`, `config`
- `/trivia-reset-cooldown` — Reset a user's trivia cooldown
- `/survey` — Manage and report on one-off anonymous DM surveys (`create`, `list`, `preview`, `delete`, `send`, `retry`, `results`, `close`, and `question add|edit|remove|move`)

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
| `DIG_CHANNEL_ID` | None | Gates `/dig` commands to this channel and routes public dig output there |
| `LOBBY_CHANNEL_ID` | None | If set, lobby embeds post here instead of the command channel |
| `LOWSKILL_LOBBY_CHANNEL_ID` | None | Optional Whine & Cheese lobby channel; falls back to `LOBBY_CHANNEL_ID` |
| `PET_CHANNEL_ID` | None | Gates the entire pets feature: without it the `/pet` cog is not loaded and pet hooks no-op |

### Advanced Configuration

Additional settings can be configured in `.env` (see `config.py` for all 200+ options):

**Lobby:**
- `LOBBY_READY_THRESHOLD`, `LOBBY_MAX_PLAYERS` - Lobby size settings

**Betting:**
- `LEVERAGE_TIERS` - Available leverage options (default: 2,3,5; 10x is always allowed on top)
- `MAX_DEBT` - Maximum negative balance (default: 500)
- `BET_LOCK_SECONDS` - Betting window duration (default: 1200 / 20 min)
- `AUTO_BLIND_ENABLED`, `AUTO_BLIND_THRESHOLD`, `AUTO_BLIND_PERCENTAGE` - Auto-liquidity settings
- `AUTO_SPECTATOR_BET_ENABLED`, `AUTO_SPECTATOR_BET_COUNT`, `AUTO_SPECTATOR_BET_TOP_COUNT`, `AUTO_SPECTATOR_BET_TOP_PERCENTAGE`, `AUTO_SPECTATOR_BET_PERCENTAGE` - Rich spectator auto-wager tiers (default: 10 total; ranks 1–5 at 2%, ranks 6–10 at 1%); spectators may also `/bet` both teams

**Economy:**
- `LOAN_COOLDOWN_SECONDS`, `LOAN_MAX_AMOUNT`, `LOAN_FEE_RATE` - Loan system
- `BANKRUPTCY_COOLDOWN_SECONDS`, `BANKRUPTCY_PENALTY_GAMES` - Bankruptcy settings (default penalty: 3 games)
- `TIP_FEE_RATE` - Tipping fee (default: 1%)
- `VANITY_TAX_RATE` - Profit tax on members without a server nickname (default: 10%)
- `DISBURSE_MIN_FUND`, `DISBURSE_QUORUM_PERCENTAGE` - Disbursement voting
- `PINGEDASH_TARGET_USER_ID`, `PINGEDKEVIN_TARGET_USER_ID` - Discord user IDs
  targeted by the paid `/shop pingedash` and `/shop pingedkevin` commands

**Wheel of Fortune:**
- `WHEEL_COOLDOWN_SECONDS` - Time between spins (default: 24 hours)
- `WHEEL_TARGET_EV` - Target expected value per spin (default: -27.5)

**Rating:**
- `OFF_ROLE_MULTIPLIER`, `OFF_ROLE_FLAT_VALUE_PENALTY` - Off-role effective
  value adjustments (defaults: 0.95 and 100)
- `OFF_ROLE_FLAT_PENALTY` - Goodness penalty per off-role player (default: 550)
- `ROLE_MATCHUP_DELTA_WEIGHT` - Weight applied to the lane matchup and role
  parity deltas when scoring teams (default: 0.18)
- `EXCLUSION_PENALTY_WEIGHT` - Goodness penalty per excluded player (default: 80)
- `RECALIBRATION_COOLDOWN_SECONDS` - Time between rating resets

**Trivia:**
- `TRIVIA_COOLDOWN_SECONDS` - Time between trivia questions (default: 6 hours)
- `TRIVIA_ANSWER_TIMEOUT_SECONDS` - Per-question answer timer (default: 15)

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

**Commands not showing:** Wait a few minutes for Discord to sync, or use `/admin sync` (admin only)

**Database issues:** Only run one bot instance. Back up the database and diagnose a
disposable copy; never delete or run reset experiments against deployed data.

**Match enrichment failing:** Check OpenDota availability and verify `OPENDOTA_API_KEY` if one is configured.

## License

This project is for the Camaraderous Dota 2 league.
