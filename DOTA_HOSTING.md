# Automated Dota hosting

Current rollout scope: **hosting**. Use one dedicated Steam account for public South Central league lobbies, invitations, side enforcement, automatic launch, game-state betting closure, match-ID capture, and verified result recording. Bot spectating, observer-client automation, and live-stat publishing are deferred. Human spectators remain allowed. No observer account or Dota game client is required by the hosting worker.

The Rust runtime can use one dedicated Steam account to host league games after a Cama shuffle or draft. It freezes each player's primary linked Steam account, creates a public lobby with no password, invites the ten players, checks their sides, requests the Dota server, closes betting when gameplay starts after the hero draft, and records a verified result through Cama's existing ratings and economy pipeline. A separate task archives the replay when Valve provides one. Multiple pending matches wait for the same account; the host remains reserved until its current match has been recorded and its lobby has been released.

Players can accept their invitations or join the public lobby and choose their assigned side. Steam's lobby protocol cannot move arbitrary human players into specific slots. The bot returns shuffled players on the wrong side to the unassigned pool and kicks visitors who occupy a Radiant/Dire playing slot. Visitors may remain unassigned or spectate; they do not prevent launch. As soon as all ten shuffled accounts are correctly seated, the next host check rechecks the roster and starts the Dota server automatically. Checks run every five seconds; there is no additional seating countdown. Open Dota's settings and allow lobby invites from non-friends. The dedicated account stays outside the ten playing slots.

## Account and league preparation

Use the dedicated Steam account with Dota initialized, Steam Guard enabled, and league-administrator access granted by the league owner. Configure each Discord guild's league with `/enrich setleague`. That command stores the league ID; Valve league permission must already exist on the account. Each participant needs a primary Steam account linked in Cama before hosting can begin. Alternate linked accounts are not invited automatically.

Set these deployment variables (hosting remains off unless explicitly enabled). For this hosting rollout, leave `DOTA_STEAM_WEB_API_KEY`, `DOTA_LIVE_BIND`, `DOTA_LIVE_TOKEN`, and `DOTA_GSI_TOKEN` unset, and use the base Compose file without `docker-compose.dota-live.yml`:

```dotenv
DOTA_HOST_ENABLED=true
DOTA_HOST_GUILD_IDS=123456789012345678
DOTA_STEAM_USERNAME=dedicated_account_login
DOTA_BOT_ACCOUNT_ID=123456789
DOTA_STEAM_SESSION_PATH=/app/data/steam/session.json
DOTA_HOST_START_AFTER=1789000000
DOTA_SERVER_REGION=31
DOTA_GAME_MODE=2
DOTA_TV_DELAY=3
DOTA_LOBBY_TIMEOUT_SECONDS=1800
DOTA_REPLAY_DIRECTORY=/app/data/replays
DOTA_REPLAY_MAX_BYTES=536870912
DOTA_REPLAY_RETENTION_DAYS=30
```

`DOTA_BOT_ACCOUNT_ID` is the 32-bit Dota account ID, not Steam64. The runtime checks that the authenticated account matches it. `DOTA_HOST_START_AFTER` must be the Unix timestamp when automatic hosting was first enabled (`date +%s`); keep that value across restarts. The number above is illustrative. Older pending matches remain available for manual handling. Missing player links or league configuration defer hosting until fixed.

`DOTA_SERVER_REGION` defaults to **31: US South Central (`dfw`, Dallas)**. This was verified directly in `scripts/regions.txt` from installed Dota client build `25219194` on 2026-09-10; its matchmaking-group value `1` is not the lobby region ID. Supported game modes are 1 (All Pick), 2 (Captains Mode, default), and 22 (Ranked All Pick). Cama's saved first-pick side is applied to the lobby. TV delay uses Valve's [current wire enum](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_common_lobby.proto): 0 = 10 seconds, 1 = 60 seconds, 2 = 120 seconds, 3 = 300 seconds (default), 4 = 900 seconds. The bundled Rust protocol names predate the 60-second option; the adapter preserves current numeric values explicitly. Lobby safety settings disable cheats and bots and enable spectating.

Run the one-time login command interactively, with the password available only to that invocation:

```sh
read -s DOTA_STEAM_PASSWORD
export DOTA_STEAM_PASSWORD
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime --bin cama-rust -- steam-login
unset DOTA_STEAM_PASSWORD
```

Set `DOTA_STEAM_USERNAME` and the session path for that shell first. Approve Steam Guard in the mobile app or enter the requested code. `DOTA_STEAM_GUARD_CODE` can supply a one-time code for bootstrap. The runtime uses the persisted session without interactive prompts; if Steam revokes it, rerun `steam-login`. Keep the session and machine-token files on the persistent data volume. The adapter writes them atomically with owner-only permissions and rejects symlink credential files. It also rejects `SSLKEYLOGFILE` so the Steam TLS connection cannot export its session keys. The normal `serve` process does not need a Steam password.

For Docker, bootstrap with the same image, user, environment, and persistent data volume that will run `serve`; pass the password with `docker compose run --rm -e DOTA_STEAM_PASSWORD bot steam-login`. Remove the temporary password from the shell afterward. Do not run two Steam hosts using the same account.

## Lifecycle and recovery

Each attempt is stored in `dota_sessions`, separately from `pending_matches`, so settlement cleanup does not erase the Valve lobby/match identity. The game-server ID is also persisted for live-feed recovery. A partial unique index reserves one active session per bot account. Every progress update checks its revision. A lost create response is reconciled by lobby identity before another request is attempted. A lost launch response is observed rather than blindly resent. Previously created sessions retain their saved region, visibility, and password; new defaults apply to new sessions.

Eligible hosted matches keep betting open while queued, gathering players, loading, and drafting. Their original timed deadline is preserved but does not close the hosted window. The worker closes betting transactionally on confirmed playable pregame (`PRE_GAME`, game-rules state 4); the 0:00 horn (`GAME_IN_PROGRESS`, 5) or postgame (6) also confirms that point has passed. Hero selection, strategy time, team showcase, and a lobby merely being `RUN` do not close it. Unknown or missing state does not authorize a guessed timer cutoff. Stale state updates cannot reopen the window. Discord displays describe the gameplay cutoff and timed last-call reminders are suppressed during the draft. Cancellation before server launch restores the timed betting policy. A known match continues to be observed for betting closure while hosting is paused for operator review; validated spectator confirmation also works during a GC connection error.

Admins retain `/admin extendbetting` as an explicit override. It adds the requested minutes to the later of the current deadline or now, including after automatic closure. An extension during the draft keeps betting open until both gameplay has started and the extension has expired; an extension after gameplay starts reopens a timed window with countdowns and reminders. The override is persisted separately from the observed gameplay marker, so later host updates and restarts cannot cancel it. Wager validation enforces its exact deadline even if the host is offline. Recorded matches cannot be reopened.

Final launch checks and result recording share an exclusive lease with manual recording and abort operations. Wagers are rejected transactionally once the Cama match is recorded, even if a cleanup retry has left its pending row behind. It verifies the league and the exact ten accounts and sides against the completed Valve match before recording. If match details are unavailable, a matching GC postgame lobby with all ten accounts and an explicit winner can supply that evidence. Incomplete details are polled again; temporary Cama settlement errors receive five attempts before operator review. Missing winners in a completed result, abnormal results, changed rosters, or a foreign lobby pause automatic handling. Replays and unavailable live feeds do not block settlement.

Inspect or resolve a session from the host with the same `DB_PATH` as the bot:

```sh
cama-rust dota-host status
cama-rust dota-host resume GUILD_ID PENDING_MATCH_ID
cama-rust dota-host cancel GUILD_ID PENDING_MATCH_ID
```

Status omits Steam credentials and lobby passwords. Resume acknowledges the reported problem and requests another reconciliation; inspect Dota and the Cama result first. Cancel requests cleanup of a lobby that has not started and preserves the Cama pending match for normal record/abort handling. Once launch has been requested, cancellation cannot discard the game or its postgame evidence. Existing `/record` winner/abort choices remain available for shuffled and drafted matches, and `/admin correctmatch` can correct a recorded winner. Automatic reconciliation adopts a matching manual result without settling again; a conflicting outcome pauses for review instead of overwriting the admin's result. The bot account remains reserved for unresolved active sessions so another match cannot overwrite them.

## Deferred: live statistics

This section documents optional work retained for later. It is not part of the current hosting rollout.

The bot's GC connection supplies lobby state, server ID and match ID. It does not itself receive a full scoreboard. To try Valve's published league/realtime feeds, set `DOTA_STEAM_WEB_API_KEY`. Public visibility and a league ID do not guarantee that Valve publishes a custom lobby to these feeds. Requests are shared and cached rather than issued for each viewer. Saved match/server IDs restore the live registration after restart, even if the lobby object has disappeared while the result is pending.

The hosting bot stays outside the playing slots, normally in the unassigned pool. Setting its GC team to `SPECTATOR` does not connect it to the running game: the [lobby team message](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_client_match_management.proto) only changes a membership slot. A GC bot [continues receiving lobby updates after launch without joining the game](https://github.com/Arcana/node-dota2/blob/master/docs/api.md#launchpracticelobby). Actual observation has a separate [watch-game handshake and game-server connection](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_client_watch.proto). Spectator-slot placement is therefore not a launch requirement or a substitute for a Dota observer client.

To expose the latest snapshot, configure:

```dotenv
DOTA_LIVE_BIND=127.0.0.1:8787
DOTA_LIVE_TOKEN=replace_with_a_random_secret_of_at_least_32_characters
DOTA_GSI_TOKEN=replace_with_a_different_random_secret_of_at_least_32_characters
```

The authenticated read route is `GET /matches/GUILD_ID/PENDING_MATCH_ID`, with `Authorization: Bearer <DOTA_LIVE_TOKEN>`. It reports the source and observation time so consumers can distinguish stale data from a current game. Known matches are registered for internal observation before betting closes; the public snapshot and Discord scoreboard remain hidden until the atomic betting close. Authenticated, match-correlated spectator GSI can confirm `map.game_state` when the GC phase is missing. Prefer the direct GC phase because spectator delay can make GSI confirmation arrive late; no draft-duration estimate or horn countdown is used. Betting closure is durable once committed. GSI samples received just before a process crash are not journaled; if closure had not committed, recovery waits for fresh GC evidence or the next spectator heartbeat (10 seconds in the example below). A live pilot must verify delivery and latency of the GC game-state transitions, especially after reconnects. Docker users can add `-f docker-compose.dota-live.yml`; its port binds to localhost on the host. Use TLS through a reverse proxy or a private tunnel for remote access.

For detailed telemetry when Valve's feeds are unavailable, a running Dota client can send spectator/observer Game State Integration to `/gsi`. A second account is a workaround for independent game sessions, not a proven requirement of the lobby-host role. Our GC host already reports Dota as running; starting an independent desktop Dota session on that account can cause Steam session contention. A single client controlling the lobby and observing its game, or a compatible headless TV consumer within the host session, still needs live qualification. See [the same-account investigation](DOTA_LOBBY_AUTOMATION_RESEARCH.md#same-account-hosting-and-observation-validation-on-2026-09-10). The host does not currently launch or drive an observer game client. Join that client as a spectator before automatic match launch, or use Dota's watch flow after the match ID is assigned. Add a file such as `gamestate_integration_cama.cfg` to Dota's `game/dota/cfg/gamestate_integration/` directory:

```text
"Cama"
{
    "uri" "http://127.0.0.1:8787/gsi"
    "timeout" "5.0"
    "buffer" "0.1"
    "throttle" "1.0"
    "heartbeat" "10.0"
    "auth" { "token" "YOUR_DOTA_GSI_TOKEN" }
    "data"
    {
        "provider" "1"
        "map" "1"
        "player" "1"
        "hero" "1"
        "items" "1"
        "abilities" "1"
        "buildings" "1"
    }
}
```

Launch that Dota client with `-gamestateintegration`. The URI must reach the collector from the observer machine; localhost works only when they share a host. An observer can supply both teams' statistics; a playing client's feed generally covers its own perspective. The collector correlates the match ID and known accounts, authenticates the payload, and never republishes its token. Spectator delay still applies.

## Replay storage and validation

Completed match details expose replay availability, cluster and salt. Available replays are downloaded into a temporary file, checked for the bzip2 header and configured size bound, synced, and atomically published as `MATCH_ID.dem.bz2`. SHA-256, size and archive time are saved with the session. Downloads retry independently for up to two days from session creation; not-recorded and expired states are retained. The configured retention policy prunes old numbered replay files when another archive is processed; 0 disables pruning. This archives game demos, not rendered video.

The repository's dependency gate currently blocks CI: 94 package versions in the expanded dependency graph still need complete deployment audit evidence. Published, pinned audits cover part of the graph; no new exemptions were introduced. See [the exact dependency audit findings](DOTA_DEPENDENCY_AUDIT.md). Passing application tests does not clear that gate.

Validation on 2026-09-10: workspace formatting and Clippy (`--all-targets --all-features -- -D warnings`) pass. The locked workspace suite passed 9,374 tests with 4 ignored; the final admin-control follow-ups also passed all 1,106 runtime-engine tests (1 ignored) and 25 match-runtime repository tests. Unit/integration fixtures cover public visitor handling, South Central discovery, immediate launch admission, betting through queued/loading/draft phases, GC and spectator gameplay confirmation, stale lobby phases, GC disconnects, operator review, admin betting extensions before adoption and after gameplay, exact extension expiry, recorded-match wager rejection and closed displays, manual winner recording during an active hosted session, automatic settlement, restart recovery without a lobby, and current TV-delay wire values. The hosting deployment pilot still needs the actual dedicated account: create a public South Central league lobby with ten linked players, check invitations, visitor handling, sides, and automatic launch, verify betting remains open through the hero draft and closes on the GC pregame transition, complete a game, and verify Cama settlement and replay availability. Observer and live-stat qualification is deferred. That external behavior cannot be verified using offline fixtures.

See [the research and source links](DOTA_LOBBY_AUTOMATION_RESEARCH.md) for Valve/Steam interfaces, existing host implementations, and account/league guidance.
