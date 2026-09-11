# Automated Dota hosting

Current rollout scope: **hosting**. Use one dedicated Steam account for public South Central league lobbies, invitations, side enforcement, automatic launch, game-state betting closure, match-ID capture, and verified result recording. Observer-client automation is deferred. Optional spectator text announcements use a separate Valve Web API feed. Human spectators remain allowed. No observer account or Dota game client is required by the hosting worker.

The Rust runtime can use one dedicated Steam account to host league games after a Cama shuffle or draft. It freezes each player's primary linked Steam account, creates a public lobby with no password, invites the ten players, checks their sides, requests the Dota server, closes betting when gameplay starts after the hero draft, and records a verified result through Cama's existing ratings and economy pipeline. The host remains reserved until its current match has been recorded and its lobby has been released. A new shuffle or captain draft created while the account is occupied automatically uses manual hosting, normal timed betting, and manual `/record`. It never queues for later bot adoption.

Players can accept their invitations or join the public lobby and choose their assigned side. Steam's lobby protocol cannot move arbitrary human players into specific slots. The bot returns shuffled players on the wrong side to the unassigned pool and kicks visitors who occupy a Radiant/Dire playing slot. Visitors may remain unassigned or spectate; they do not prevent launch. As soon as all ten shuffled accounts are correctly seated, the next host check rechecks the roster and starts the Dota server automatically. Checks run every five seconds; there is no additional seating countdown. Open Dota's settings and allow lobby invites from non-friends. The dedicated account stays outside the ten playing slots.

## Account and league preparation

Use the dedicated Steam account with Dota initialized, Steam Guard enabled, and league-administrator access granted by the league owner. Configure each Discord guild's league with `/enrich setleague`. That command stores the league ID; Valve league permission must already exist on the account. Each participant needs a primary Steam account linked in Cama before hosting can begin. Alternate linked accounts are not invited automatically.

Set the following deployment variables (hosting remains off unless explicitly enabled). Use the base Compose file. Live spectator announcements reuse the existing `STEAM_API_KEY`; hosting and postgame statistics do not. The HTTP/GSI endpoint variables are optional and are not needed for the built-in Valve polling worker:

```dotenv
DOTA_HOST_ENABLED=true
DOTA_HOST_GUILD_IDS=123456789012345678
DOTA_STEAM_USERNAME=dedicated_account_login
DOTA_STEAM_PASSWORD=your_dedicated_account_password
DOTA_BOT_ACCOUNT_ID=123456789
DOTA_TV_DELAY=0
```

`DOTA_BOT_ACCOUNT_ID` is the 32-bit Dota account ID, not Steam64. The runtime checks that the authenticated account matches it. Compose persists the session at `/app/data/steam/session.json`; region defaults to 31, game mode to 2, and lobby timeout to 1800 seconds. Hosting eligibility is frozen transactionally when a new pending match is created. Historical pending matches without an explicit reservation stay manual, while existing hosted sessions retain restart recovery. No activation timestamp or deployment test-mode setting is required.

`DOTA_SERVER_REGION` defaults to **31: US South Central (`dfw`, Dallas)**. This was verified directly in `scripts/regions.txt` from installed Dota client build `25219194` on 2026-09-10; its matchmaking-group value `1` is not the lobby region ID. Supported game modes are 1 (All Pick), 2 (Captains Mode, default), and 22 (Ranked All Pick). Cama's saved first-pick side is applied to the lobby. TV delay uses Valve's [current wire enum](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_common_lobby.proto): 0 = 10 seconds, 1 = 60 seconds, 2 = 120 seconds, 3 = 300 seconds (default), 4 = 900 seconds. The bundled Rust protocol names predate the 60-second option; the adapter preserves current numeric values explicitly. Lobby safety settings disable cheats and bots and enable spectating.

Normal startup uses `DOTA_STEAM_PASSWORD` when no valid saved session exists. Start the bot normally with `docker compose up -d bot` or `cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime --bin cama-rust -- serve`. If Steam requests a Guard code, set `DOTA_STEAM_GUARD_CODE` to the current email or authenticator code and restart. Mobile approval can be completed in the Steam app when requested. Guard codes are temporary; remove the code after successful login. Later startups reuse the saved session.

Keep passwords in the deployment's private environment and keep the session and machine-token files on the persistent data volume. The adapter writes these files atomically with owner-only permissions and rejects symlink credential files. It also rejects `SSLKEYLOGFILE` so the Steam TLS connection cannot export its session keys. Do not run two Steam hosts using the same account.

The interactive `steam-login` command remains available as an optional way to prepare the session in advance, including `docker compose run --rm bot steam-login` with the same image, user, environment, and data volume as `serve`.

## Discord hosting controls

Use `/shuffle` for per-match hosting choices: it is where the roster and match
settings are frozen. `/lobby` continues to manage the Cama matchmaking queue.
Administrators can supply any of these optional `/shuffle` fields:

| Option | Purpose |
| --- | --- |
| `hosting` | Bot hosting, or Manual to create and run Dota yourself |
| `server_region` | Dota server-region ID; `31` is the tested local default |
| `game_mode` | All Pick, Captains Mode, Random Draft, Single Draft, All Random, Least Played, Captains Draft, Ability Draft, All Random Deathmatch, Ranked All Pick, or Turbo |
| `first_pick` | Radiant, Dire, or random draft first pick; does not swap players between teams |
| `start` | Automatic when all ten are ready, or wait for an admin |
| `tv_delay` | Dota's delay enum: 0=10 seconds, 1=60 seconds, 2=120 seconds, 3=300 seconds, 4=900 seconds |
| `league_id` | Override the server's configured positive league ID |
| `visibility` | Public or unlisted |

For a manual match, use `/shuffle hosting:Manual`. Cama still shuffles, displays
teams, supports betting with the normal timed window, and accepts `/record`.
The Dota host never adopts that match. Bot hosting remains subject to the
deployment's hosting enablement and guild allowlist.

`/admin dota settings` shows saved guild overrides. Supply the same settings
fields to update defaults for **future shuffles**. Per-shuffle overrides take
precedence; existing matches retain their saved settings. `/admin dota reset`
clears these overrides and restores deployment defaults and the configured league.
Regular players can shuffle using guild defaults; specifying hosting overrides
requires an admin.

`/admin dota status` shows this server's hosting sessions. `/admin dota start`
requests launch for a lobby waiting on manual start; it retains all roster,
team, ownership, and settings checks. `/admin dota cancel` requests cleanup of
an unstarted bot lobby, and `/admin dota resume` resumes a session paused for
review. `/admin dota manual` switches a pending match to human hosting and
cleans up any owned, unstarted bot lobby while preserving the Cama match for
normal `/record`. It also restores the normal betting deadline for an unstarted hosted match.
A started or uncertain launched game cannot be discarded by manual handoff.

Operator commands accept optional `pending_match`; omitting it is allowed only
when the eligible match is unambiguous. They operate only in the invoking guild.
Bot/manual selection is frozen per pending match. Automatic fallback does not alter the match already using the bot.

## Testing

Unit and integration tests inject fake Dota transports directly; deployment always uses the real adapter when hosting is enabled. Fake Discord players do not represent Steam accounts and cannot launch a real hosted match. Keep automated local tests on a disposable database. To run a local Discord bot without making any Steam connection, leave hosting disabled.

The tested account uses league `19144`, region `31`, game mode `2`, and `DOTA_TV_DELAY=0`. The league is stored using `/enrich setleague`, not an environment variable.

## Lifecycle and recovery

Each attempt is stored in `dota_sessions`, separately from `pending_matches`, so settlement cleanup does not erase the Valve lobby/match identity. The game-server ID is also persisted for live-feed recovery. A partial unique index reserves one active session per bot account. Every progress update checks its revision. A lost create response is reconciled by lobby identity before another request is attempted. A lost launch response is observed rather than blindly resent. Previously created sessions retain their saved region, visibility, and password; new defaults apply to new sessions.

Eligible hosted matches keep betting open while gathering players, loading, and drafting only while the worker has a recent, verified observation of its owned lobby. Adoption alone does not open betting. Actual GC lobby data must be at most 45 seconds old to renew the 90-second database lease; cached reads and unrelated GC messages do not renew it. Disconnect, stale data, loss of ownership, or operator review suspends the automatic window. If the worker dies, database wager admission rejects the expired lease. A quiet lobby can therefore temporarily pause betting until another actual lobby update arrives. Their original timed deadline is preserved but does not close the hosted window. The worker closes betting transactionally on confirmed playable pregame (`PRE_GAME`, game-rules state 4); the 0:00 horn (`GAME_IN_PROGRESS`, 5) or postgame (6) also confirms that point has passed. Hero selection, strategy time, team showcase, and a lobby merely being `RUN` do not close it. Unknown or missing phase does not imply that gameplay has started; freshness expiry separately suspends wagers until observation recovers. Stale state updates cannot reopen the window. Discord displays describe the gameplay cutoff and timed last-call reminders are suppressed during the draft. Cancellation before server launch restores the timed betting policy. A known match continues to be observed for betting closure while hosting is paused for operator review; validated spectator confirmation also works during a GC connection error.

Admins retain `/admin extendbetting` as an explicit override. It adds the requested minutes to the later of the current deadline or now, including after automatic closure. An extension during the draft keeps betting open until both gameplay has started and the extension has expired; an extension after gameplay starts reopens a timed window with countdowns and reminders. The override is persisted separately from the observed gameplay marker, so later host updates and restarts cannot cancel it. Wager validation enforces its exact deadline even if the host is offline. Recorded matches cannot be reopened. `/admin dota betting action:suspend reason:...` is a stronger operator stop that also blocks extensions; `action:resume` removes only that stop and respects the normal deadline, freshness, and gameplay rules. Both actions preserve the operator, reason, and timestamp.

Final launch checks and result recording share an exclusive lease with manual recording and abort operations. Wagers are rejected transactionally once the Cama match is recorded, even if a cleanup retry has left its pending row behind. It verifies the league and the exact ten accounts and sides against the completed Valve match before recording. If match details are unavailable, a matching GC postgame lobby with all ten accounts and an explicit winner can supply that evidence. Incomplete details are polled again; temporary Cama settlement errors receive five attempts before operator review. Missing winners in a completed result, abnormal results, changed rosters, or a foreign lobby pause automatic handling. Unavailable live feeds do not block settlement. Match results are verified through GC match details or explicit postgame lobby evidence; replay files are not required.

Inspect or resolve a session from the host with the same `DB_PATH` as the bot:

```sh
cama-rust dota-host status
cama-rust dota-host resume GUILD_ID PENDING_MATCH_ID
cama-rust dota-host cancel GUILD_ID PENDING_MATCH_ID
```

Status omits Steam credentials and lobby passwords. Resume acknowledges the reported problem and requests another reconciliation; inspect Dota and the Cama result first. Cancel requests cleanup of a lobby that has not started and preserves the Cama pending match for normal record/abort handling. Once launch has been requested, cancellation cannot discard the game or its postgame evidence. Existing `/record` winner/abort choices remain available for shuffled and drafted matches, and `/admin correctmatch` can correct a recorded winner. Automatic reconciliation adopts a matching manual result without settling again; a conflicting outcome pauses for review instead of overwriting the admin's result. The bot account remains reserved for unresolved active sessions so another match cannot overwrite them.

## Interrupted setup, recording, and operator resolution

New shuffles save an incomplete setup marker before reserving money. Public betting, hosting adoption, and recording reject that marker. A failed financial setup refunds its pending wagers and seed reservations in the same transaction that removes the pending match. A crash during setup is compensated after its five-minute setup lease expires; the recovery worker checks every 30 seconds. A successfully prepared shuffle instead retries missing Discord destinations from saved receipts and stable delivery nonces. An expired interaction confirmation does not undo a published shuffle. Database participant claims prevent overlapping new shuffles across processes. Captain drafts retain their existing durable finalization job; an additional draft setup marker blocks betting, hosting, and recording until the durable financial and publication job completes. An unfinished linked draft job also blocks abort, preserving its recoverable financial plan; `/draft resume` lets an admin retry it before recording or aborting.

Abort checks the canonical record, refunds outstanding wagers and seed reservations, and deletes pending state in one immediate transaction. A simultaneous wager is either included in the refund or rejected after deletion. A recorded match cannot be aborted. Automatic blind batches store their original result atomically and return it on retry without another debit.

Recording verifies the pending identity, roster, and winner on every entry, including retries. Before the core record commits, it freezes the financial configuration, tax eligibility, and reward policy. The pending row remains a recovery work item until settlement, streaming rewards, canonical result delivery, and thread closure succeed. Saved message receipts and per-chunk delivery markers avoid repeating completed publications. Finalization and abort payloads are archived for diagnosis. `/admin correctmatch` refuses a match whose original finalization is still pending; let recovery finish before applying a correction.

Use `/admin dota status` to see pending/Cama/Dota identities, progress, betting suspension, and the saved error. Critical and terminal status messages have a durable retry outbox. Removing a server from the hosting allowlist pauses its prelaunch actions while retaining safe cancellation and reconciliation for games already launched.

Use `/admin dota resolve pending_match:ID outcome:recorded dota_match:VALVE_ID reason:...` to finish a session whose Cama result has been recorded and finalized, or `outcome:void` to refund and close an abandoned result. Supply the exact Dota ID displayed by status (`0` only when unassigned). The command records an audited intent; the worker verifies account/lobby ownership and terminal evidence before releasing the account reservation. A foreign lobby or a still-active game prevents destructive cleanup. An independently verified completed GC result can resolve a stale running phase. A conflicting recorded result must be corrected through the supported recording tools first. Resume is still appropriate for a transient fault; resolve is the explicit terminal recovery route.

## Spectator text updates and optional live statistics

React with 📻 on the **lobby message** to subscribe to that lobby's spectator updates. This does not join the playing roster. At shuffle creation, the source lobby message is frozen with the pending match; the final ten participants are excluded even if they subscribed earlier. Removing the reaction revokes access on the next reconciliation (normally 15 seconds). The worker persists subscriptions, ownership markers, event baselines, and queued messages for restart recovery.

The destination is a temporary private **text channel**, rather than the shared match thread: everyone is denied access, opted-in nonparticipants receive read-only access, and all match participants receive explicit denies. Private threads cannot provide the same isolation from thread-management roles. The bot verifies current guild membership, roles, ownership, and exact channel permissions before delivery. Discord server owners and Administrator members bypass channel denies; if any participant has either privilege, delivery is withheld and an existing owned channel is deleted. There is no shared-thread fallback. The bot needs permission to manage channels and permission overwrites. Up to 80 viewers are supported. The channel is deleted 15 minutes after the pending Cama match disappears; permission drift or unsafe membership changes can remove it sooner.

Text announcements are a scaffold over fresh, complete Valve snapshots: at least three additional scoreboard kills, a relative team net-worth change of at least 3,000, and explicitly reported building destruction. Samples are compared at most every 45 seconds; gaps above 90 game-clock seconds establish a new baseline. A kill burst is not presented as a reconstructed teamfight. Missing values, delta frames, stale snapshots, reopened betting, and unavailable upstream data suppress live announcements. The ordinary match thread now shows betting status without live scores. No extra Steam connection, observer, minimap, or third-party spectator stream is started.

Updates use a game-clock headline, compact net-worth totals, event lines, and the current score. Headlines distinguish taking the lead, extending it, and closing a deficit. Structure losses are grouped by team. Zero configured delay adds no footer noise; a positive or unknown feed delay is shown. For example:

> 📻 **23:15 — Radiant takes the lead**
> ⚔️ **Radiant 4–1 Dire** in kills over the last 45s
> 💰 **3.5k swing to Radiant** — Radiant leads by **2.5k**
> 🏰 **Dire** lost a structure
> _Score: Radiant 9–7 Dire_

**The current live-announcement implementation requires `STEAM_API_KEY`.** Without a key, no Valve polling worker starts. GC lobby state and player-perspective GSI do not supply accepted announcement frames. A simultaneously manually hosted JV match is not registered with this feed and receives no live announcements; adding the key alone does not change that. Its manual result recording and existing postgame API enrichment remain available.

Hosting alone does not supply these statistics. The feed must be independently available and qualified; without it the private channel only receives its introduction. GSI remains useful for game-phase confirmation, but a player-perspective GSI payload is not accepted as a complete match-announcement frame.

Minimap research is deferred until text delivery is qualified. [Noxville/datdota Broodmother](https://github.com/datdota/broodmother) publishes an AGPL frontend, but its backend is private and its hosted-service terms restrict redistribution. No Broodmother code, assets, or service is integrated. Other public projects such as [MiniDota](https://github.com/AdamJo/dota2-minimap-spectator) and [dota2-tracker](https://github.com/Miroscyer/dota2-tracker) still need license, maintenance, and feed-compatibility review.

The bot's GC connection supplies lobby state, server ID and match ID. It does not itself receive a full scoreboard. To try Valve's published league/realtime feeds, set `STEAM_API_KEY`. Public visibility and a league ID do not guarantee that Valve publishes a custom lobby to these feeds. Requests are shared and cached rather than issued for each viewer. Saved match/server IDs restore the live registration after restart, even if the lobby object has disappeared while the result is pending.

The hosting bot stays outside the playing slots, normally in the unassigned pool. Setting its GC team to `SPECTATOR` does not connect it to the running game: the [lobby team message](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_client_match_management.proto) only changes a membership slot. A GC bot [continues receiving lobby updates after launch without joining the game](https://github.com/Arcana/node-dota2/blob/master/docs/api.md#launchpracticelobby). Actual observation has a separate [watch-game handshake and game-server connection](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_client_watch.proto). Spectator-slot placement is therefore not a launch requirement or a substitute for a Dota observer client.

To expose the latest snapshot, configure:

```dotenv
DOTA_LIVE_BIND=127.0.0.1:8787
DOTA_LIVE_TOKEN=replace_with_a_random_secret_of_at_least_32_characters
DOTA_GSI_TOKEN=replace_with_a_different_random_secret_of_at_least_32_characters
```

The authenticated read route is `GET /matches/GUILD_ID/PENDING_MATCH_ID`, with `Authorization: Bearer <DOTA_LIVE_TOKEN>`. It reports the source and observation time so consumers can distinguish stale data from a current game. Known matches are registered for internal observation before betting closes; the authenticated snapshot and private spectator announcements remain hidden until the atomic betting close. Authenticated, match-correlated spectator GSI can confirm `map.game_state` when the GC phase is missing. Prefer the direct GC phase because spectator delay can make GSI confirmation arrive late; no draft-duration estimate or horn countdown is used. Betting closure is durable once committed. GSI samples received just before a process crash are not journaled; if closure had not committed, recovery waits for fresh GC evidence or the next spectator heartbeat (10 seconds in the example below). A live pilot must verify delivery and latency of the GC game-state transitions, especially after reconnects. Docker users can add `-f docker-compose.dota-live.yml`; its port binds to localhost on the host. Use TLS through a reverse proxy or a private tunnel for remote access.

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

## Validation

Coordinator postgame statistics are retained in the durable hosting session before recording and then saved in `match_gc_statistics`, scoped to the Cama guild/match and verified Valve match ID. Only a matching ten-player account/side roster can supply the snapshot. The snapshot contains supplied final player stats, scores, duration, items, ability upgrades and draft choices; absent protobuf fields remain absent, while legitimate zeroes are retained. An unavailable statistics payload does not prevent result settlement through the existing verified-result fallback.

Postgame enrichment prefers these coordinator fields. Existing API enrichment fills missing stats and parsed telemetry, including the actual team gold/XP advantage arrays used by the graph. Later refreshes preserve the coordinator values and previously acquired parsed data. Final GPM/XPM and net worth do not reconstruct minute-by-minute advantage graphs. The existing auto-enrichment setting still controls automatic publication.

After verified, completed GC match details supply a complete roster, the host optionally fetches Valve's small postgame metadata file and decrypts its win-probability graph using the account-authorized key. This uses the existing Steam session, without joining an observer or downloading a replay. The constructed Valve CDN URL permits an HTTP fallback when TLS fails, disables redirects, and bounds download and decompression sizes. Both Zstandard and bzip2 streams are supported. The key is never sent to the CDN, logged, or saved in match statistics.

The optional fetch has an eight-second total budget and at most two attempts; missing metadata does not prevent settlement. Successful graphs are persisted with the coordinator statistics and available through the **Win probability** button on the postgame match embed. Button clicks render saved data without contacting Steam. If metadata is not ready during that initial attempt, there is currently no durable background retry for this graph. The chart shows Radiant percentages against ordered sample indices: exact sample-to-game-time mapping remains unverified, so it does not invent timestamps. This is postgame history; the existing draft prediction remains a separate pregame estimate.

The diagnostic `steam_metadata_probe` obtains coordinates and keys using only the dedicated bot's saved session. Private probe files use mode `0600`. The offline `decode_match_metadata` example reuses the production decoder. On 2026-09-11 it decoded matches 8992390628, 8992284302 and the existing Cama match 8943080142; each supplied 64 ordered samples.

This rollout does not download, archive, or prune replay files. Existing sessions retain their historical replay metadata for compatibility; completed results do not start replay polling or archival jobs.

The repository's dependency gate currently blocks CI: 99 package versions in the expanded dependency graph still need complete deployment audit evidence. Published, pinned audits cover part of the graph; no new exemptions were introduced. See [the exact dependency audit findings](DOTA_DEPENDENCY_AUDIT.md). Passing application tests does not clear that gate.

Pre-push validation on 2026-09-11: workspace formatting and Clippy (`--all-targets --all-features -- -D warnings`) pass. The locked full workspace suite passed **9,562 tests across 52 targets**, with 4 existing ignored tests and no failures. This includes atomic admission of simultaneous shuffles, permanent manual fallback with normal betting deadlines, manual handoff releasing reservations, Draft retry hashes and withdrawal across restart, shared `STEAM_API_KEY` loading, spectator message rendering/privacy, and the prior adversarial recording/betting recovery regressions. The dependency exemption check passes; cargo-vet remains blocked on 99 unaudited package versions.

The last local Docker smoke test restarted healthy with 46 Discord commands synchronized and a passing schema/integrity check. That image predates the shared-key rename and spectator-copy rewrite; those latest changes are validated by Rust tests and have not been redeployed. Local Steam hosting remains disabled with `DOTA_HOST_ENABLED=false`; no additional Steam connection was started for pre-push validation.

Production qualification still needs a complete match with ten linked real players: verify invitations, visitor handling, sides, automatic launch, betting closure at pregame, settlement, and actual optional live-feed availability/latency. The text scaffold is not yet qualified on a real inhouse match. Hosting alone does not supply the live statistics, and actual live-feed availability still requires a real-match test. Owner/Administrator participants prevent private delivery. These external behaviors cannot be verified using offline fixtures.

See [the research and source links](DOTA_LOBBY_AUTOMATION_RESEARCH.md) for Valve/Steam interfaces, existing host implementations, and account/league guidance.
