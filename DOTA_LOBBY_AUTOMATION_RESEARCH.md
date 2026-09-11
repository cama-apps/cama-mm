# Dota lobby automation and live match data

Current scope (2026-09-10): proceed with hosting; defer bot spectating and live-stat publishing. The Rust host, durable sessions, automatic result recording, replay archive, and optional live-data collector are implemented, but the collector remains unconfigured for this rollout. See [DOTA_HOSTING.md](DOTA_HOSTING.md) for the actual configuration and routes, and [DOTA_DEPENDENCY_AUDIT.md](DOTA_DEPENDENCY_AUDIT.md) for the outstanding CI audit gate. The research and work breakdown below explain the design; live account qualification remains outstanding.

Research date: 2026-09-10. Repository baseline: `main` at `ec6fb1a3`.

Current-region verification: Valve's installed Dota client build `25219194` contains `scripts/regions.txt` in `game/dota/pak01_dir.vpk`. Its `USSouthCentral` entry has lobby region **31**, location code `dfw` (Dallas), and matchmaking group `1`. Cama uses region 31 for new lobbies; the matchmaking group is a separate field. This was a read-only inspection of game assets, without logging into Steam. Older public region tables omit this newly added location.

The [current lobby protobuf](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_common_lobby.proto) also adds a 60-second TV-delay option: wire values 0–4 now mean 10, 60, 120, 300, and 900 seconds. The bundled Rust enum predates that addition, so Cama validates and preserves the numeric values directly. The default is 3 (300 seconds); live labels use each session's saved delay.

## Recommendation

Build a Rust Steam/Game Coordinator adapter that turns a completed Cama shuffle or player draft into a hosted Dota lobby. Automate creation, configuration, invitations, roster validation, launch, match-ID capture, and result ingestion. Retain manual joining/side selection where the protocol cannot perform it, plus the existing result/abort controls for exceptional matches.

Deliver the hosting flow end to end. Qualify lobby setup, launch, GC game-state betting closure, match identity, and result recording with the dedicated account. Observer and live-stat work is deferred by the latest scope decision; related research below is retained for later. The evidence below comes from published library implementations, generated protocol definitions, and repository inspection. No Steam login, lobby creation, invitation, match launch, or live API qualification was performed during this research.

Treat “record match” as two separate outcomes: recording the winner into Cama, and optionally archiving the Dota replay. Neither requires recording a video stream.

## Steam account guidance and permission

Follow-up official-source check, 2026-09-10: Steam's [Subscriber Agreement, section 4.C](https://store.steampowered.com/subscriber_agreement/#4) broadly prohibits automation interacting with Steam content/services; section 4.D permits account restrictions or termination. Technical support in community libraries does not establish Valve authorization, and adding a league administrator does not itself establish an automation exception. No published Dota lobby-bot account exemption has been identified in this research. Seek clarification from Valve/Steam Support about this specific league-hosting use before treating it as an officially permitted deployment. This corrects the earlier implication that account creation and league permissions alone resolve all requirements.

For ordinary account setup, Steam's [limited-account guidance](https://help.steampowered.com/en/faqs/view/71D3-35C2-AD96-AA3A) says eligible purchases or wallet funding totaling at least US$5 remove restrictions such as initiating friend requests and accessing the Steam Web API. That payment is not a bot license or proof that Dota lobby invitations require spending. [Steam Guard](https://help.steampowered.com/faqs/view/7EFD-3CAE-64D3-1C31) supports multiple accounts in one authenticator app; keep authentication enabled. Account creation should be manual.

For an existing league, Valve's [league administration instructions](https://help.steampowered.com/en/faqs/view/072E-47E8-B513-6E4E) say to open the league's ellipsis menu, select Admin, and add the account's Steam Community profile URL under League Administrators. This is the technical league-access step, separate from the automation-policy question.

### How existing services do it

FACEIT explicitly documents its lobby bots: players receive an invitation, join the assigned side, are removed from an incorrect team, and the game starts once everyone joins. See [FACEIT's connection guide](https://support.faceit.com/hc/en-us/articles/207569569-How-do-I-connect-to-a-DOTA2-match). Its [invite troubleshooting guide](https://support.faceit.com/hc/en-us/articles/207570689-No-invite-to-DOTA2-match-room-lobby), updated August 2026, says players must disable the setting that blocks non-friend lobby invitations. Add this check to our invitation troubleshooting; friend requests are not inherently required for every lobby invite.

Valve itself named FACEIT as an organizer of The International 2015 open qualifiers in its [official announcement](https://store.steampowered.com/news/posts/?appids=570&enddate=1432851622&feed=steam_community_announcements). This establishes historical official involvement, not the terms of its current bot authorization. Section 2.G of the Subscriber Agreement expressly contemplates written Valve consent for third-party matchmaking; FACEIT's actual permission terms are not established by the public sources reviewed here. Do not assert that it has a special bot-account license, or that smaller league hosts inherit permission.

Smaller community projects also document the ordinary dedicated-Steam-account/league-admin approach, for example [inhouse-bot-2.0's automated lobby setup](https://github.com/Samsquamptch/inhouse-bot-2.0/wiki/Automated-Lobbies). That project's suggestion to disable MFA is implementation-specific and should not be adopted; use supported authentication/session handling while keeping Steam Guard enabled. These examples demonstrate a deployed engineering pattern but do not resolve the public-policy ambiguity.

## Capability assessment

| Requested step | Assessment | Implementation and remaining condition |
| --- | --- | --- |
| Create a Dota lobby | Supported by existing clients | Authenticated Steam account communicates with Dota's Game Coordinator. Cama creates a public practice lobby without a password and verifies the resulting lobby snapshot. |
| Invite the selected ten players | Supported | Invite SteamID64 identities; each player still accepts/joins in Dota. Missing links, alternate accounts, offline players, and invitation delivery need handling. |
| Set league, region, mode, password | Supported, subject to account permissions | Apply the configured league ID and settings; verify the GC accepted them. Existing client documentation says the bot should be a league admin. Cama's `/enrich setleague` does not grant Valve permissions. |
| Set first pick / flip Radiant and Dire | Supported, with mode-specific semantics | Map Cama's final side/first-pick decisions into Dota settings. Whole-team flipping is available. Cama's player draft is distinct from Dota's hero draft. |
| Place arbitrary humans into exact slots | Not established | Standard team-slot requests act on the requesting account; they do not include a target player ID. Plan for players to select their assigned side, with automatic verification and correction prompts. |
| Start the match | Supported for lobby host | Launch after an exact-roster/settings check and readiness policy. A launch request is not proof that a server started; observe the following lobby state. |
| Capture match ID and detect completion | Protocol support exists | Observe lobby updates and persist the Valve match ID and outcome. Qualify actual delivery and reconnect behavior with our account. |
| Automatically record Cama result | Feasible integration | Validate the hosted match identity, final roster, sides, and terminal outcome, then call the production recording/settlement workflow once. Detailed statistics can arrive later. |
| Archive replay | Conditional | Retrieve replay metadata after completion and download when available. Replay generation/access/retention must be tested for our lobby configuration. |
| Expose live stats | Several tiers | Lobby status is the simplest. Spectatable-game summaries or live league scoreboards may provide more without a game client. Rich spectator telemetry requires additional qualification/infrastructure. |

The existing [node-dota2 API](https://github.com/Arcana/node-dota2#readme) documents create/configure/invite/flip/launch operations and says the hosting bot can continue receiving lobby updates without joining the game server. Current [match-management messages](https://github.com/SteamTracking/Protobufs/blob/master/dota2/dota_gcmessages_client_match_management.proto) expose lobby settings, first-pick configuration, team-slot selection, kick-from-team, and launch. These establish an implementation route, not a live service guarantee.

## Existing work worth using

| Project | Useful role | Assessment |
| --- | --- | --- |
| [steam-vent](https://docs.rs/steam-vent/latest/steam_vent/struct.GameCoordinator.html) | Rust transport used by the implementation | Published version inspected: 0.5.0, dated July 6, 2026. Provides GC handshake, typed message send/request/receive primitives. Cama adds the Dota lobby orchestration and state-cache handling. |
| [steam-vent-proto-dota2](https://docs.rs/steam-vent-proto-dota2/latest/steam_vent_proto_dota2/) | Rust protocol types | Version inspected: 0.5.2. Includes Dota messages and a Dota GC handshake. Pin compatible versions and compare required definitions with the current protocol snapshot. |
| [node-dota2](https://github.com/Arcana/node-dota2) | Historical lobby API reference | Archived/deprecated. Direct examples of the operations we want; useful for implementing the Rust adapter. Its documented API is not evidence of current end-to-end compatibility. |
| [go-dota2](https://github.com/paralin/go-dota2) | Current behavior and GC state-cache reference | Implements lobby operations and shared-object cache subscriptions; [recent history](https://github.com/paralin/go-dota2/commits/master) includes August 2026 updates. Useful guidance for snapshot/update/reconnect handling. |
| [dota-ihl-bot](https://github.com/devilesk/dota-ihl-bot) | Existing inhouse product reference | Combines Discord queues, team selection, Dota hosting, and match tracking. Its documented Node 10/PostgreSQL 9.5 baseline makes it a reference rather than a drop-in dependency. |
| [Dota2SentinelBot](https://github.com/ErkoKnoll/Dota2SentinelBot) | End-to-end hosting/result reference | A separate example of automated hosting and postgame tracking; qualify maintenance and protocol compatibility before borrowing code. |

The [old steam-vent GitHub repository](https://github.com/icewind1991/steam-vent) is archived because development moved to Codeberg. The July 2026 published Rust package is stronger maintenance evidence than the archived mirror alone. This still needs a real Dota handshake and lobby test.

The repository requires Rust for production. A Node/Python/Go bot can inform the implementation; adopting one as a production sidecar would change the project's current architecture policy. No such change is assumed here.

These clients implement a reverse-engineered GC interface, rather than a supported public lobby-management API. Budget for protocol updates and compatibility checks after Dota patches.

## Expected player flow

1. Existing Cama shuffle or captain draft finishes and persists its pending match.
2. A worker reserves an available host account and creates the corresponding Dota lobby.
3. It applies league/region/mode/first-pick settings, public visibility, and an empty password, and moves the host account out of the ten player slots. Spectators and unassigned visitors may stay; visitors occupying a playing side are kicked.
4. It invites the ten chosen Steam accounts and updates the existing Discord match thread with joined/missing/wrong-side status and joining instructions.
5. Players accept the invitations and choose their assigned sides where needed. Readiness is based on a verified Dota roster, not Discord online presence.
6. Once all ten correct accounts are on their expected sides, readiness is satisfied, and settings match, the worker launches the server. Betting stays open through the Dota hero draft and closes when playable pregame is confirmed.
7. The worker persists the Valve match ID as soon as it is observed and updates match status. The hosting rollout does not publish a live scoreboard.
8. A trustworthy terminal result enters Cama's existing recording pipeline; OpenDota enrichment and replay download retry independently.

Do not use Dota's built-in balanced shuffle to seat players: it would replace Cama's team balancing. Whole-team flipping is useful only when it preserves the intended roster and Cama's recorded side mapping.

## Live match data

### Tier 1: hosted-lobby status

The [client lobby schema](https://github.com/SteamTracking/Protobufs/blob/master/dota2/dota_gcmessages_common_lobby.proto) includes members, pending invites, lobby state, game state, server identity, match ID, start time, duration, and match outcome. These support joining/loading/in-progress/finished status without a rendered game client. They do not constitute a full live per-player scoreboard. Fields are optional: missing outcome is not a Dire win, and losing the GC connection is not a match abort.

### Tier 2: summaries and league scoreboards without a game client

The [spectating discovery protocol](https://github.com/SteamTracking/Protobufs/blob/master/dota2/dota_gcmessages_client_watch.proto) supports querying by league or explicit lobby IDs. `CSourceTVGameSmall` contains scores, game time, heroes, Radiant lead, building state, spectators, update time, and delay. This is a promising lightweight scoreboard source to test with our hosted league lobby. Being the host does not prove the match will appear in the spectating feed.

Also qualify Steam's `IDOTA2Match_570/GetLiveLeagueGames/v1` with a Steam Web API key and our league ID. The [tracked Steam API definition](https://github.com/SteamTracking/SteamTracking/blob/master/API/IDOTA2Match_570.json) supports league and match filters. Check whether the real match appears, which fields are populated, update cadence, and delay. Do not promise private-lobby coverage or zero-delay data merely because a request or schema exists.

For richer data without a spectator client, test `IDOTA2MatchStats_570/GetRealtimeStats/v1` using the hosted lobby's server Steam ID. The [current API definition](https://github.com/SteamTracking/SteamTracking/blob/master/API/IDOTA2MatchStats_570.json) requires `server_steam_id`; the [associated realtime schema](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_common.proto) includes detailed player, economy, item, ability, and position data. This is worth testing before provisioning GSI infrastructure. Neither source guarantees access for an ordinary private lobby: treat missing/error responses as an unavailable feed, and measure actual delay.

A [May 2026 report on Valve's issue tracker](https://github.com/ValveSoftware/Dota2-Gameplay/issues/32617) describes missing live backpack/neutral-item/enchantment information and failures of Web API match details. This is a user's observed failure report, not a verified universal outage; it reinforces the need to measure our actual feed and avoid making unattended completion depend on a single Web API endpoint.

Proposed display: score, game clock, heroes, available economy/objective data, and explicit source/update time/delay. Mark stale data visibly; missing fields stay unavailable. Poll once per relevant source/league and share the cached result among displays. A 15–30 second poll and similarly throttled Discord message edit is a starting application policy, subject to measured freshness and service limits.

The implementation exposes an optional authenticated Rust endpoint, `GET /matches/{guild_id}/{pending_match_id}`, backed by the normalized cache. It returns the Valve match ID, source, observation time, source/configured delay, freshness, and available scoreboard/player fields. The pending-match identity remains stable before and after Cama result recording. A deployment token restricts access and responses omit lobby credentials. Viewer requests never cause additional upstream polling.

### Tier 3: rich spectator telemetry

The spectator-slot investigation found no basis for requiring the GC host to occupy a spectator slot. The current [GC enum](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_shared_enums.proto) defines `SPECTATOR=3` and `PLAYER_POOL=4`, but slot assignment only changes lobby membership. The [watch-game protocol](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_client_watch.proto) separately returns connection information, a TV secret, and optionally a broadcast URL. A consumer must actually receive and decode the game stream to obtain observer telemetry; the GC response alone does not supply it. That consumer could be the Dota client or a compatible headless implementation. Keep the current GC host unassigned until the observer path is qualified.

[dota-gsi](https://github.com/tomasfarias/dota-gsi) demonstrates Rust ingestion of Dota Game State Integration, including configuration and the `-gamestateintegration` launch option. This route requires a running Dota client sending HTTP game-state updates. A headless Steam/GC session does not produce GSI payloads.

A dedicated spectator client is the candidate for rich player/items/abilities/map data. A player client has a different perspective and should not be assumed to expose both teams' complete state. Qualify spectator access, payload completeness, match-ID correlation, and delay before committing to the data model. Full-client startup, updates, reconnects, display/runtime resources, and capture health are a separate operational project.

### Same-account hosting and observation: validation on 2026-09-10

**Source validation does not establish that a second account is mandatory.** It establishes that our present GC-only host is not a complete observer, and that starting an independent game session with the same account is a different problem from one client hosting and observing its own lobby.

- **Independent GC bot plus desktop Dota on one account:** our `steam-vent` dependency sends `ClientGamesPlayed` for app 570 during GC setup. The bot already occupies a game session. [`node-steam-user`](https://github.com/DoctorMcKay/node-steam-user#playingstate) documents blocking when the account plays in another session; [Valve's auth results](https://partner.steamgames.com/doc/api/steam_api#EAuthSessionResponse) also describe the game session being disconnected after another login. Changing a logon ID can resolve a duplicate-login collision, but does not demonstrate concurrent game-session support. This is not a qualified deployment arrangement.
- **One account, one actual Dota client:** the candidate is to create and control the lobby through that client, place the host in a broadcaster/observer position, and collect its GSI. The [lobby schema](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_common_lobby.proto) stores leadership independently from member teams and broadcaster channels. This supports testing the arrangement; schema structure is not proof of a successful current-client match. It requires a client-control adapter and client lifecycle/reconnect handling that Cama does not currently implement.
- **One GC session plus a headless TV consumer:** request normal `CMsgWatchGame` while retaining the hosted lobby, then verify whether access is granted and lobby ownership survives. A `READY` response only confirms connection information. Game-stream transport, decoding, and delay still need validation. The optional HTTP broadcast URL is a candidate, not a guarantee that our league lobby publishes a usable HTTP stream.
- **Existing “spectator bots”:** inspected [`node-dota2-spectator`'s handler](https://github.com/TJRoger/node-dota2-spectator/blob/master/handlers/sourcetv.js) and [`dota2-webspectator`](https://github.com/jackys-95/dota2-webspectator). They issue GC discovery/friend-spectate requests and consume coordinator or Web API data. They do not demonstrate a current full Source 2 observer retaining our hosted lobby. The former is archived.
- **Do not substitute Dota Plus friend watching:** `CMsgSpectateFriendGameResponse` has explicit errors for the spectator already being in a lobby and for league games. Those restrictions belong to that request; they do not establish the behavior of normal `CMsgWatchGame` or joining the match as an admitted lobby broadcaster.

The live proof should use a disposable, unrated test game: (1) confirm the host remains leader after selecting its observer/broadcaster position; (2) launch and observe draft-to-pregame transitions, match ID, and both teams' GSI; (3) measure actual observer delay; (4) reconnect and confirm ownership, observation, and result reconciliation survive. Separately test a normal watch request from the existing headless host and any returned broadcast feed. A successful watch response without live game data is not a passing observer test. Test outside Cama's production ratings/betting database.

No dedicated bot login was configured in the current checkout/environment during this validation. No Steam login, lobby, or live game was run. The account requirement therefore remains **unproven**, and the next decision should follow the live proof rather than assume two accounts.

Do not treat server-to-GC scoreboard message definitions as a client subscription API. Do not make rich live stats depend on OpenDota's postgame parsing pipeline.

OpenDota's [current specification](https://api.opendota.com/api) exposes `/live` for top ongoing games and `/request/{match_id}` for replay parsing. Its live list is not a guaranteed feed for our private games; retain the existing OpenDota integration for postgame data. STRATZ is an optional alternative to evaluate only if our games are ingested; its [GraphQL API](https://stratz.com/api) does not remove the need to prove private-match coverage. Adding another stats vendor is unnecessary for this hosting implementation.

## Result details and replay archive

After capturing the Valve match ID, request GC match details with retry/backoff; the [current Go client's generated API](https://github.com/paralin/go-dota2/blob/master/client_generated.go) includes `RequestMatchDetails`. Qualify GC details as the primary source for completed hosted games, alongside terminal lobby evidence, with OpenDota as asynchronous enrichment and recovery. Do not require the replay parser to finish before recording a validated winner.

The [Dota replay utility](https://dota2.readthedocs.io/en/stable/dota2.utils.html) builds replay URLs from match ID, cluster, and replay salt returned by match details. Archive the `.dem.bz2` when it becomes available; store its source metadata and a checksum, set a retention policy, and report unavailable/expired replays explicitly. There is no need for a lobby “record video” command. A video recording would require a running viewing client and a separate capture pipeline.

Current [match protocol definitions](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_gcmessages_common.proto) distinguish replay available, not recorded, and expired. OpenDota's [replay URL implementation](https://github.com/odota/core/blob/master/svc/util/utility.ts) is another reference for download construction. Do not infer that every completed practice lobby necessarily has an accessible replay.

## Cama integration and correctness

### Existing implementation anchors

| Concern | Current code | Integration implication |
| --- | --- | --- |
| Persisted match roster, sides, first pick, betting times | [`PendingMatchState`](rust/crates/cama-db/src/match_runtime.rs) | Start from the durable match produced by shuffle/draft, rather than the mutable Discord queue. |
| Shuffle finalization | [`MatchHandler::finalize_shuffle`](rust/crates/cama-runtime/src/match_provider.rs) | Enqueue hosting only after successful pending-match creation. |
| Captain draft completion | [`draft_provider.rs`](rust/crates/cama-runtime/src/draft_provider.rs) | Trigger the same hosting workflow after final side/first-pick decisions and player selection. |
| Linked Steam identities | [`opendota_player.rs`](rust/crates/cama-db/src/opendota_player.rs) | Existing global account links supply identity; hosting needs one selected account per participant. |
| Manual result and production completion | [`finalize_record` / `record_match_blocking`](rust/crates/cama-runtime/src/match_provider.rs) | Share this orchestration with a trusted background result source. |
| Atomic rating/match persistence | [`record_match_core_atomic`](rust/crates/cama-db/src/core_repositories.rs) | Preserve the production rating implementation and retry identity. |
| Postgame discovery and validation | [`match_discovery.rs`](rust/crates/cama-app/src/match_discovery.rs) | Reuse validation/enrichment; direct capture of the Valve ID reduces ambiguous history searches. |
| League setting and enrichment composition | [`enrichment_provider.rs`](rust/crates/cama-runtime/src/enrichment_provider.rs) | Reuse guild configuration and the existing OpenDota client/limiter. |
| Runtime wiring | [`main.rs`](rust/crates/cama-runtime/src/main.rs) | Register the adapter and durable worker with existing startup/recovery/health mechanisms. |

The existing [`lobby_service.rs`](rust/crates/cama-app/src/lobby_service.rs) manages Discord queues, readiness, and thread/message state. There is currently no Steam GC session or Valve lobby identifier in that flow. The existing [`guild_config.league_id`](rust/crates/cama-db/src/guild_config.rs) is metadata for discovery/enrichment, not evidence of a configured Valve lobby.

### Persistent orchestration

Add a durable Dota session record associated with `(guild_id, pending_match_id)`, separate from the Discord queue. Store a host-account reference, Dota lobby ID, Valve match ID, expected roster and selected Steam identities, final sides, settings, lifecycle, command attempts, and last observed status. Keep the session after pending-match cleanup so result/replay recovery still has its identifiers.

Use distinct types for Cama match ID, pending-match ID, Valve match ID, Dota lobby ID, Steam account ID, and SteamID64. A player can have alternate Steam links: freeze one account per participant for each hosted session instead of inviting every linked account or silently picking an arbitrary alternate.

Implement policy in `cama-domain`/`cama-app`, persistence and migrations in `cama-db`, and the Steam adapter/worker in `cama-runtime`. A narrow typed lobby port should expose create/configure/invite/observe/launch/close operations. Preserve the single Rust database writer and avoid blocking the Discord interaction on Steam login or GC responses.

Suggested lifecycle:

```text
Pending match → Create requested → Lobby observed → Gathering players
             → Ready → Launch requested → Running → Outcome observed
             → Result recorded → Enrichment/replay complete
```

Failures and reconnects are recoverable states alongside that lifecycle. Persist commands before execution and reconcile external state after uncertain results; a timeout must not blindly create a second lobby or launch again. Use per-host account leases and keep the initial design at one active hosted session per account until releasing the account during a running game has been tested.

### Result processing

Extract or expose the production recording orchestration for both manual commands and verified external outcomes. The runtime currently couples much of this to an interaction responder. Automated completion must retain rating updates, betting settlement, rewards, pending cleanup, postmatch hooks, and enrichment scheduling.

**Do not wire automation into `cama-db/src/match_recording.rs::record_match_atomic`.** Its own comment identifies it as a test double with fixed ±32 rating changes. Production reaches `core_repositories::MatchRepository::record_match_core_atomic` through the runtime's `record_match_blocking`/`finalize_record` workflow.

Use the durable pending-match identity and a Valve-match association to prevent duplicates. Serialize automatic result, manual result, and abort paths using the existing durable safeguards, and preserve conflict evidence for review. A replayed event must not settle bets twice. A match that never started, has an unknown outcome, or returns insufficient identity evidence should retain the manual resolution path.

### Betting and visibility

The requested cutoff is actual gameplay after the Dota hero draft. Hosted and eligible queued matches use a persisted marker to keep betting open beyond the old `bet_lock_until` deadline. Close under `BEGIN IMMEDIATE` when the owned GC lobby reports `PRE_GAME` (4), `GAME_IN_PROGRESS` (5), or `POST_GAME` (6), or when authenticated, match-correlated GSI confirms the same phase. Map loading (10) and team showcase (8) are not numerically earlier than pregame, so do not use an ordered numeric comparison. Hero selection (2), strategy time (3), and player draft (12) are also excluded. These values come from the current [Valve game-state enum](https://github.com/SteamTracking/GameTracking-Dota2/blob/master/Protobufs/dota_shared_enums.proto). A launched server or `RUN` lobby can still be drafting. Suppress timed reminders and refresh the wager display around the state-driven close. Unknown state is not a reason to guess a draft cutoff; spectator delay can make a GSI fallback late and must be qualified in the pilot. Preserve the current ability to record games manually when hosting is disabled or unavailable. The bot's spectators and any published scoreboard must follow the configured delay; private lobby credentials do not belong in public stats payloads.

## Qualification and delivery plan

These are engineering estimates, not externally sourced delivery commitments. They assume one experienced developer, timely access to a host account and league permissions, and no major protocol incompatibility.

| Phase | Work | Rough effort / exit condition |
| --- | --- | --- |
| 1. Live qualification | Rust login + GC handshake; create/configure lobby; move bot to non-player slot; invite; verify sides and first pick; launch; observe match ID/result; test stats and replay availability | 2–3 developer days initially. Exit with redacted evidence of a completed hosted match and an explicit capability matrix. Extend if Steam/protocol behavior needs repair. |
| 2. Production hosting | Durable worker/state, identity validation, settings, invitations, readiness, betting lock, launch/retry/reconnect, Discord thread status and recovery controls | Approximately 1–2 weeks after successful qualification, including the shared recording refactor and result recovery below. |
| 3. Automatic results | Exact match association, shared production finalization, race/idempotency tests, delayed enrichment, manual conflict recovery | Deliver with phase 2; qualify terminal outcomes before enabling unattended settlement. |
| 4. Basic live stats / replay archive | Selected GC or league feed, freshness display, optional replay download and retention | Approximately 2–4 additional days if the qualified feeds cover our games. |
| 5. Rich live telemetry | Spectator client operation plus GSI ingestion and richer views | Separate estimate after spectator/payload proof; not required for lobby automation. |

Minimum live checks: Steam Guard and restart login; valid/invalid league permission; missed invite; alternate account; unexpected player; wrong side; host outside player slots; whole-team flip; launch failure; GC reconnect before/after launch; normal finish; abandoned/no-result game; unavailable/delayed match details; duplicate completion; two queued sessions competing for a host; actual live-feed visibility/delay; replay availability.

Required implementation tests: deterministic fake-GC state transitions, roster/settings validation, snapshot/update handling, retry reconciliation, host leases, migrations, guild isolation, draft-versus-gameplay betting ordering, admin betting overrides, automatic/manual/abort races, once-only production settlement, and restart recovery. Run the repository's Rust formatting/lint/test gates for implementation. This research established the implementation below; see [DOTA_HOSTING.md](DOTA_HOSTING.md) for the implemented runtime and deployment controls. The existing `/admin extendbetting` command remains an explicit timed override before or after automatic betting closure; recorded matches cannot be reopened. Manual winner recording, aborts, and winner corrections remain available alongside hosting.

Prerequisites for the first live spike: a dedicated Steam account authorized to host under the intended Dota league, initial Steam Guard authentication with securely persisted session material, ten known player account mappings, the league ID/region/mode, and a coordinated test match. Add a Steam Web API key if qualifying the league feed. Existing Cama/OpenDota credentials alone do not establish a Steam GC session.
