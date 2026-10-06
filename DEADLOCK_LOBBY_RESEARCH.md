# Deadlock lobby implementation research

Research date: 2026-10-02. Repository originally inspected at `0fe792c8af9ebe2b0a100ef1222a3b6e899f6c07`. This records the research and implementation proposal. Subsequent implementation, including the dedicated `#deadlock-mm` channel, is described in [DEADLOCK_HOSTING.md](DEADLOCK_HOSTING.md); consult that document for the actual configuration and supported commands. Public documentation and source were inspected; no authenticated rating requests, real game lobbies, or live match completion were tested.

The first release should provide one persistent Deadlock queue per guild, **defaulting to Street Brawl 4v4**, with a **shuffle option for Standard 6v6**, external initial ratings, balanced teams, and **integrated Jopacoin betting**. Use a **separate Deadlock Steam account** and a dedicated Rust host adapter built on the existing Steam infrastructure. Keep the two modes' local ratings separate. Hero drafting, player drafting, and captain drafting are out of scope.

Live Deadlock host behavior and result access remain qualification items, with manual result/abort recovery required even when hosting is automated. Betting is part of the first release, including when a result requires manual verification.

This is feasible, but changing the existing ten-player threshold is insufficient. Cama's Dota implementation couples queue identity, registration, role assignment, pending matches, ratings, recovery, and economic effects to Dota. An additive Deadlock implementation with a few deliberately shared primitives has a smaller regression surface than converting every Dota feature into a generic game framework.

## Recommended scope

| Capability | First release | Reason |
| --- | --- | --- |
| Register a Deadlock player and select a linked Steam account | Yes | Must work for someone who has never registered for Dota. |
| Pull an initial external rating | Yes | Statlocker first choice after confirming source mode and season; Valve rank through Deadlock API is a fallback. |
| Persistent Discord queue and join/leave buttons | Yes | Core equivalent of the existing lobby experience. |
| Street Brawl default and Standard shuffle option | Yes | One queue; default eight-player match, explicit twelve-player override. |
| Ready check and fair waitlist selection | Yes | Eight or twelve confirmed players, with predictable treatment of extras. |
| Balanced team assignment | Yes | A small role-free optimizer can support both formats. |
| Local ratings and match history | Yes | Imported ratings initialize the system; subsequent inhouse results should improve it. |
| Manual result and abort | Yes | Keeps games usable when a provider is unavailable or cannot observe a custom match. |
| In-game party creation and joining code | Dedicated Deadlock account integration | Reuse Steam auth/transport, add Deadlock protocol and durable worker; qualify live. |
| Automatic result recording and wager settlement | Target after verified final-result qualification | Manual audited resolution remains a required recovery route. |
| Pool betting, shared JC wallet, leverage, payouts and refunds | Required | Core first-release functionality, with game-aware market identity. |
| Automatic liquidity and investments | Include with explicit game scope | Preserve economic rules; existing Dota preferences/fund earmarks must not silently extend to Deadlock. |
| Hero drafting, player drafting, captain drafting | Excluded | User explicitly excluded drafting. |
| Lane optimization, participation rewards, pets, referrals, fantasy stats | Outside initial scope | Separate from required betting; avoid accidental Dota hooks. |
| Spectator telemetry, replays, match enrichment | Later | Not required to gather players and run balanced games. |

“Standard” here names the 6v6 ruleset. It does not mean that a private Cama match participates in Valve's public Standard or Ranked ladder. Keep ruleset, public source ladder, matchmaking category, and local rating pool distinct.

## Existing implementation audit

There are two different lobbies: the Discord queue that gathers players and the in-game lobby hosted through Steam. Cama already implements both for Dota. Most reusable infrastructure is below their Dota-specific policies.

The physical source paths below remain authoritative even where workspace slices compile those files through `#[path]` or Cargo library path overrides. Some comments still mention the historical Python migration; current runtime wiring and `CLAUDE.md` establish Rust as the production implementation.

| Concern | Repository evidence | Deadlock implication |
| --- | --- | --- |
| Queue categories | [`LobbyKind`](rust/crates/cama-app/src/embeds.rs), line 13, contains only Open/LowSkill. [`LobbyScope`](rust/crates/cama-app/src/dedicated_lobby_channel.rs), lines 41–64, maps these to IDs 1/2. | These identify Dota eligibility categories, not game formats. |
| Restart decoding | [`lobby_service.rs`](rust/crates/cama-app/src/lobby_service.rs), lines 315–325; [`lobby_manager.rs`](rust/crates/cama-app/src/lobby_manager.rs), lines 99–110; [`readycheck.rs`](rust/crates/cama-app/src/readycheck.rs), line 224. | Every non-2 lobby ID is interpreted as Open. Simply adding ID 3 can recover as a Dota lobby. |
| Capacity configuration | [`lobby_provider.rs`](rust/crates/cama-runtime/src/lobby_provider.rs), lines 103–139. | Ready threshold and maximum capacity are provider-wide. Changing environment values is not per-format configuration. |
| Ready check | [`readycheck.rs`](rust/crates/cama-app/src/readycheck.rs), line 21, fixes the minimum at 10; lines 296–312 recognize Dota activity. | Eight-player readiness requires new policy and game-aware presentation. Discord activity is not proof of in-game readiness. |
| Registration | [`lobby_provider.rs`](rust/crates/cama-runtime/src/lobby_provider.rs), lines 3658–3675, requires preferred Dota roles. [`player_mmr_fallback.rs`](rust/crates/cama-app/src/player_mmr_fallback.rs), line 255, fetches OpenDota and initializes Dota ratings. | Deadlock enrollment cannot reuse those entry points unchanged. |
| Team representation | [`team.rs`](rust/crates/cama-domain/src/team.rs), lines 9 and 140–146, fixes five roles and five players. | A configurable threshold cannot make this model represent four or six players. |
| Balancing | [`shuffler.rs`](rust/crates/cama-domain/src/shuffler.rs), lines 34, 433, 1138–1145, 1701–1725, and 2292–2306. | Fixed role arrays, ten-player selection, and five-player partitions require a separate role-free policy. |
| Runtime shuffle | [`match_provider.rs`](rust/crates/cama-runtime/src/match_provider.rs), lines 2524–2532; [`shuffle_setup.rs`](rust/crates/cama-runtime/src/match_provider/shuffle_setup.rs), lines 39, 61–85, and 209–219. | Runtime repeats ten-player and Dota role requirements. Domain changes alone are insufficient. |
| Existing Dota captain draft | [`draft.rs`](rust/crates/cama-app/src/draft.rs), lines 38–39 and 261–283. | Leave Dota behavior intact; no Deadlock draft implementation. Existing Dota player claims still need interoperability. |
| Pending match | [`match_runtime.rs`](rust/crates/cama-db/src/match_runtime.rs), lines 43–103. | Radiant/Dire, role/value, first-pick, and economic fields have no typed game discriminator. |
| UI and components | [`embeds.rs`](rust/crates/cama-app/src/embeds.rs), lines 508–608; [`lobby_provider.rs`](rust/crates/cama-runtime/src/lobby_provider.rs), lines 384–427. | Dota sides/stat links and Open/LowSkill component IDs need dedicated Deadlock rendering and routing. |
| Queue eviction | [`lobby_runtime.rs`](rust/crates/cama-app/src/lobby_runtime.rs), lines 321–336. | Current cross-queue behavior enumerates the two Dota queues. Deadlock will not be included automatically. |
| Hosting | [`dota_lobby.rs`](rust/crates/cama-domain/src/dota_lobby.rs), lines 43–83; [`cama-steam`](rust/crates/cama-steam/src/lib.rs), line 34; [`lobby.rs`](rust/crates/cama-steam/src/lobby.rs), lines 22–46. | Admission requires 5v5 and transport uses app ID 570 plus Dota protobufs. Reuse Steam session infrastructure selectively, not Dota wire messages. |
| Host result validation | [`dota_host.rs`](rust/crates/cama-runtime/src/dota_host.rs), lines 1739–1760. | Dota's ten-account result validation is not a generic match verifier. |

There is worthwhile reuse: typed Discord providers and acknowledgments, readable guild-name resolution, durable thread/message publication patterns, clocks and seeded entropy, HTTP adapters, SQLite migrations and transaction policy, and pure Glicko arithmetic. Reuse those through narrow ports. Preserve the Dota optimizer and host behavior.

### Storage and result recording are the larger boundary

The [canonical schema](rust/schema/canonical_schema.sql) combines Dota rating and economy state in `players` (line 1354), keyed by `(discord_id, guild_id)`. `matches` (958), `rating_history` (1453), `lobby_state` (586), and `pending_matches` (1074) lack game identity. `lobby_type` means shuffle/draft; `lobby_kind` means open/lowskill; the existing match `game_mode` is Dota enrichment. None is a spare Deadlock discriminator.

The real production recorder is [`match_provider.rs`](rust/crates/cama-runtime/src/match_provider.rs), lines 4202–4625, calling [`record_match_core_atomic`](rust/crates/cama-db/src/core_repositories.rs), line 4076. It writes Dota results, player ratings, histories, and pairings. Runtime finalization then settles bets and currency, handles loans, and invokes game-related hooks. Dota rating corrections also reconstruct historical matches from those tables in [`match_correction.rs`](rust/crates/cama-db/src/match_correction.rs), lines 2405, 2423, and 2649.

**Do not use `match_recording.rs::record_match_atomic` as the production shortcut.** Its [implementation](rust/crates/cama-db/src/match_recording.rs), line 1162, explicitly identifies it as a test double with fixed ±32 ratings. The similarly named application recording helper does not replace the runtime's production path.

Add isolated Deadlock enrollment, queue, match, rating, and history storage, plus a game-neutral betting-market boundary that shares the existing JC wallet and economic rules. A later unified match schema is possible, but it must migrate and scope every relevant reader, replay, leaderboard, enrichment worker, and economic hook. Adding a `game` column while leaving old queries unfiltered is unsafe. Betting makes wallet-only enrollment and market identity first-class work rather than optional cleanup.

## Dynamic formats and the player flow

Use a typed format catalog rather than arbitrary user-configurable team sizes:

| Format ID | Team size | Required players | Local rating pool |
| --- | ---: | ---: | --- |
| `deadlock_standard_6v6` | 6 | 12 | `deadlock_standard` |
| `deadlock_street_brawl_4v4` | 4 | 8 | `deadlock_street_brawl` |

Persist Street Brawl as the queue default. An explicit shuffle override selects Standard for that pending attempt and its resulting match; a new attempt with no override defaults to Brawl again. This per-attempt interpretation avoids making one Standard game silently change future defaults. Persist a complete format snapshot on each committed match and betting market: game, format/version, team size, roster, sides, rating pool, and settings. Completion must never consult the queue's current mode to score an older match.

Proposed commands are `/shuffle lobby:deadlock` for Brawl and `/shuffle lobby:deadlock format:standard` for 6v6. Add Deadlock to lobby routing and introduce `format`; do not repurpose the existing `mode` option, which already means Balanced/Region Split, or Dota's numeric `game_mode`. In an unambiguous Deadlock context the lobby can be inferred. Reject incompatible Dota-only options rather than ignoring them. Registration/history/admin utilities can live under `/deadlock`; there is no separate mode-setting command required and no draft command.

The queue message should show the selected mode, confirmed/required count, waitlist, and any provisional rating status. Notify near the required count using format policy; Dota's hardcoded eight/nine-player notifications are not correct for both formats. Defer slow Discord interactions before database, HTTP, or CPU work.

Recommended sequence:

1. Join the guild's Deadlock queue. Existing Steam links can be selected; newcomers enroll without Dota roles or OpenDota.
2. Shuffle defaults to Brawl; `format:standard` requests twelve players. Validate target availability without silently falling back to Brawl.
3. Run or validate a readiness generation for that exact queue revision and requested format. Eight Brawl confirmations do not authorize a twelve-player Standard game.
4. Select eight or twelve ready players fairly, acquire shared player claims, and balance them.
5. Commit the pending match, rosters, seed/rating snapshots, market identity, and durable economic setup plan. Finish automatic wagers/funding once before opening public betting and publishing teams.
6. The dedicated host creates the private party, posts joining instructions, verifies participants, and closes betting durably before launch. Manual hosting is an explicit busy/failure fallback with a defined betting lock.
7. Record a verified/manual whole-game result and settle wagers exactly once, or abort/refund the market. Release the match's claims through its durable finalization lifecycle.
8. Keep unselected players queued with their waiting priority. Rejoining selected players for another game is explicit.

### Mode changes

The shuffle option selects the target format before roster selection or economic setup. Preserve membership and join times. For an override that changes an active readiness attempt, increment its revision, cancel old readiness/pruning/notification jobs, publish the requested format, and require fresh confirmations. Every format-sensitive interaction carries the queue ID and revision/generation, so late clicks cannot confirm or launch an old format. If fewer than twelve eligible players exist, reject the Standard attempt without creating a market or spending funds.

The existing [`ReadycheckGeneration`](rust/crates/cama-app/src/readycheck.rs), lines 338–350, has no format or queue revision. Its reset operation removes the entire scope; extract an invalidate-generation operation that preserves membership rather than rebuilding a queue incidentally.

Do not switch a reserved or committed match in place. Once any manual or automatic wager exists, changing mode, players, or sides requires aborting/refunding that unstarted market and creating a fresh shuffle/market identity. A subsequent attempt can select a different format while an earlier match runs because the earlier match owns an immutable snapshot.

Reaching eight players does not automatically launch; the shuffle action still selects/commits the game. Reaching twelve does not automatically promote the format to Standard. Omitted format means Brawl, regardless of queue count.

| Queue situation | Proposed behavior |
| --- | --- |
| Fewer than eight ready | Wait. |
| Eight to eleven ready, Standard requested | Reject the attempt with the required count; leave the gathering queue intact. |
| Eight ready, Brawl selected | Start one 4v4. |
| Nine to eleven ready, Brawl selected | Select eight; preserve the rest on the waitlist. |
| Twelve ready, Standard selected | Start one 6v6. |
| Twelve ready, format omitted | Brawl; select eight. Standard requires the explicit shuffle option. |
| More than required | Fair roster selection precedes team balancing. Queue capacity and match size are separate settings. |

For an initial capacity, matching the current operational scale of roughly fourteen queued players is reasonable, but this is a proposed Deadlock default, not a claim that every old lobby helper uses fourteen. Avoid arbitrary pool growth until selection behavior is measured.

### Team balancing and waiting fairness

Select the playing roster by readiness, waiting priority, and previous exclusions; then optimize the split of that fixed roster. If the optimizer simultaneously chooses who sits out to minimize rating difference, unusually strong or weak players can repeatedly be excluded.

With equal team sizes, minimize the absolute difference in total/mean local rating. Randomize equivalent solutions and final sides using injected entropy. No Dota positions, off-role penalties, purchased avoids, region-split mode, or currency-based skill estimate are needed. Region selection can be an explicit queue setting initially.

There are only `C(8,4)/2 = 35` unordered 4v4 splits and `C(12,6)/2 = 462` unordered 6v6 splits. These are exact combinatorial counts, not benchmark results. Exhaustive search of a fixed roster is straightforward. Searching all rosters from fourteen players would increase the work to 105,105 candidates for 4v4 and 42,042 for 6v6; it also couples bench fairness to balance unnecessarily.

One rating cannot fully account for hero skill or composition. The MVP should make that limitation clear without adding a hero-rating model before there is enough local data.

## Initial ratings and provider research

### Statlocker

The public API documents `GET /api/public/profile/{accountId}` and `POST /api/public/profiles` (up to 100 IDs). Profile fields include `ppScore`, `estimatedRankNumber`, `lastUpdated`, and calibration information. Authentication uses `X-API-Key`; access applications require Steam sign-in and manual review. Published limits are 10,000 account items/hour and 1,000 match items/hour; batches count items. It requests attribution. The profile contract does not document a mode/era selector. [API documentation](https://statlocker.gg/api).

Direct page retrieval returned a generic application shell. I also inspected its publicly served documentation module, `8305.0a8566b5.chunk.js`, linked from the live application bundle, to verify the endpoint descriptions rather than relying solely on indexed snippets. No authenticated calls were made.

A concrete inconsistency needs resolution: its example pairs 4,250 PP with badge 84, while its published conversion computes badge 76. Do not copy that conversion into production as a verified current contract. This is an observed documentation inconsistency, not proof that actual API responses are wrong. [Live documentation module](https://statlocker.gg/static/js/8305.0a8566b5.chunk.js).

More importantly, Statlocker's August 31 changelog says Standard and Ranked ladders use different scales, while the August/September updates describe season-specific and population-weighted scoring. That makes a field called `ppScore` insufficient without knowing which ladder and era it represents. The changelog also describes a fix for Brawl winners previously being inferred incorrectly from round scores. [Statlocker changelog](https://statlocker.gg/changelog).

Statlocker has a separate Brawl PP leaderboard, with twenty scored Brawl matches required to appear. That establishes a Brawl-specific rating exists on the website; it does not establish that the documented profile endpoint exposes it. [Brawl leaderboard](https://statlocker.gg/brawl-leaderboard).

**Recommendation:** use Statlocker if access and mode/season semantics can be confirmed. Capture real authorized fixtures for calibrated, uncalibrated, inactive, Brawl-only, and unknown accounts before committing to a mapping. Treat obtaining access as an external dependency; no key application was submitted during this research.

### Deadlock API as an alternative

The current contract deprecates `/v1/players/mmr`; the old estimated MMR is gone. Prefer `GET /v1/players/rank?account_ids=...` or `/v1/players/{account_id}/rank`. These report Valve rank derived from a ranked match, rather than a general skill estimate for every player. [Current OpenAPI specification](https://api.deadlock-api.com/openapi.json).

The rank implementation returns account IDs with badge/tier/subrank and match provenance. Batch results are not ordered; protected accounts are omitted. Unranked/missing rank is represented by zeros and a null last match. The batch maximum is 1,000; documented limits are 20 requests/minute per IP, 100/minute and 2,000/hour per key, and 200/minute globally. Eternus uses special badge semantics, so raw rank progress should not be interpreted as a uniform skill scale across all tiers. [Rank endpoint source](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/players/rank.rs).

Use this as a coarse ranked-skill prior when Statlocker is unavailable, with explicit source labeling. It will not solve Brawl-only coverage. Do not retain an older integration's interpretation of `player_score` merely because a deprecated route still responds.

Other tracker websites are not automatically independent providers. Statlocker itself directs raw-data consumers to Deadlock API. Prefer these documented interfaces over scraping UI pages or relying on undocumented endpoints.

### Proposed rating policy

Use one local Glicko-based algorithm initially, with independent Standard and Brawl state. Pure Cama arithmetic aggregates the actual team length in [`rating.rs`](rust/crates/cama-domain/src/rating.rs), lines 315 and 515. Its surrounding Dota seed mapping is unsuitable: lines 282 and 382–402 map 0–12,000 Dota MMR to 0–3,000 and apply a 500-MMR discount. Deadlock should construct a normalized rating directly with its own configuration.

External values are initialization evidence, not a continuously authoritative replacement for local results:

1. Prefer a sufficiently recent, calibrated source for the target mode and verified era.
2. For Standard, fall back to Valve ranked badge if PP is unavailable or ambiguous.
3. For Brawl, use actual Brawl data when the supported API exposes it. Otherwise use a labeled weak prior from Standard, with higher uncertainty and shrinkage toward the neutral rating, or a neutral provisional seed.
4. If no usable data exists, allow enrollment with a provisional neutral prior and high uncertainty. An audited admin override can fix an obviously unsuitable seed.
5. Freeze the initialized seed. Re-registration, relinking, provider recovery, or a later refresh must not overwrite established local ratings.

A fallback between providers needs its own calibrated transformation. Do not mix raw PP, badge codes, rank positions, and Cama points in the same optimizer. Badge codes are discontinuous: a tier's sixth badge is followed by the next tier's first badge. A contiguous ordinal can preserve order, but it still does not prove equally spaced skill.

The normalization spike should define a versioned monotone transform per provider/mode/era into a common Cama scale, anchored to fixed reference values. Keep its constants fixed for a season/configuration version, rather than recomputing them from whoever happens to join tonight. Verify example player ordering and predicted win probabilities before choosing the spread. There is not enough evidence in public docs alone to claim a statistically validated PP-to-Glicko conversion.

Missing data is not a zero rating. Distinguish unavailable/private, never calibrated, stale, provider error, throttling, and malformed response. Missing timestamps or counts remain unknown; do not manufacture certainty from them. A Steam account's calibration count is not automatically a reliable local Glicko RD.

Cache provider snapshots by account/source/mode/era with fetched and source timestamps. Use bounded timeouts, exponential backoff with jitter, retry headers, short negative caching, and a stale-but-labeled fallback. Fetch outside SQLite write transactions. Keep joining and shuffling functional during provider outages after enrollment. Map batch responses by account ID, even if one provider currently promises order.

For a small population, separate pools will calibrate slowly. Weak shared initial priors are a reasonable compromise; updating both pools after every Brawl result would instead make Brawl farming move Standard ratings. Record one outcome for an entire Street Brawl game, not one independent rating update per round.

## In game lobby hosting

### Dedicated Deadlock account and Rust adapter

**Recommended path under the separate-account assumption:** retain the existing Dota host and add a `DeadlockSteamClient` and dedicated worker. Both accounts can have independent Steam connections and coordinator sessions. Reuse generic authentication/transport, while keeping lobby state, protocol types, account leases, and recovery game-specific. No shared Steam-session manager is needed. Shared player reservations and shared-wallet transaction protection remain necessary.

The new account needs its own Deadlock access; Dota access does not confer it. Valve's store page still describes friend-invite access. No account entitlement was checked during this research. [Valve store page](https://store.steampowered.com/app/1422450/Deadlock/?l=english).

[`SteamAuth`](rust/crates/cama-steam/src/auth.rs), around line 244, already implements token reuse, password fallback, Steam Guard, redaction, and private atomic persistence. Reuse it with distinct expected account ID and credentials. Prefer `data/steam/deadlock/session.json` and `data/steam/deadlock/machine_tokens.json`, leaving existing Dota paths alone. A subtle issue at line 96: two differently named session files in the same directory still default to the same `machine_tokens.json`; concurrent token-store updates should not share that file. Validate configured account and path separation at startup.

There is a published Rust `steam-vent-proto-deadlock` 0.1.0 package, released June 16, 2026. It depends on `steam-vent-proto-common` 0.5.1, matching Cama's dependency family. Inspection of its public crate archive confirmed a `GCHandshake` for app ID **1422450**, generated private-party messages, and Normal/StreetBrawl enum values. Its generated protobuf version check is 3.5.1, matching the current lockfile. This is source-level compatibility evidence; it has not been added to or compiled in Cama. Pin and audit it, compare its protocol snapshot with current definitions, and update/generate missing types before shipping. [Published crate](https://docs.rs/crate/steam-vent-proto-deadlock/latest).

The existing Rust Deadlock API service is concrete implementation evidence for private-party creation, spectator placement, readiness, and leaving. Its example uses spectator slot 31, which should be verified rather than inferred from Dota slots. [Creation source](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/create.rs), [party utilities](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/utils.rs).

Required adapter operations are create a private party in the frozen mode/region, obtain its join code, invite where permitted, observe membership/readiness, assign or validate sides/slots, remove unauthorized playing members where permitted, launch, track match identity/lifecycle, retrieve final metadata, and leave/clean up. Existence of protocol actions is not proof every operation is permitted for our account. Normal hero selection stays in the game; no Cama hero/pick/ban draft is introduced.

Client-version handling needs its own adapter. The provider queries `IGCVersion_1422450/GetClientVersion/v1/`, reads the minimum accepted version, caches it, and falls back to tracked `steam.inf`. Avoid embedding a version from a successful one-off test. [Version retrieval implementation](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/services/steam/client.rs).

Preserve two useful practices from [`DotaSteamClient`](rust/crates/cama-steam/src/lobby.rs), lines 600–698: retain the Steam connection so its heartbeat stays alive, and establish subscriptions before applying welcome/cache hydration. Deadlock needs its own shared-object decoding, snapshot freshness, deletion/delta handling, and reconnect reconciliation; reusing Dota's parser would be incorrect even where the envelope looks similar.

Reserve the dedicated account atomically when committing the pending match, including both existing active sessions and not-yet-adopted pending reservations, as [Dota routing](rust/crates/cama-db/src/dota_host_routing.rs) already does. Start with one active hosted match per Deadlock account until safe earlier release is proven. Keep the account reserved through ambiguous launch/result/cleanup states. If it is occupied, explicitly select manual hosting or refuse the new hosted attempt before opening its market; do not silently adopt a later game when the account becomes free.

For automatic results, `CSOCitadelLobby` supplies lifecycle and match identity, not a direct winner. `GetMatchMetaData` returns replay/metadata location information, not a ready-to-settle result object. The adapter must retrieve/decode the actual final metadata and validate frozen accounts, mode, teams, completion, and winner. Keep audited manual result/abort fallback. A postmatch signal alone cannot authorize a payout. [Client messages](https://raw.githubusercontent.com/SteamTracking/Protobufs/master/deadlock/citadel_gcmessages_client.proto), [lobby object](https://raw.githubusercontent.com/SteamTracking/Protobufs/master/deadlock/citadel_gcmessages_common.proto).

A headless GC host is the implementation target; neither a graphical game client nor a spectator replay parser should be assumed necessary for lobby creation. The live spike must still establish which final-result permissions a spectator host receives. Direct hosting avoids the third-party service's fixed fifteen-minute bot lifetime and the inbound endpoint needed if using its optional callbacks; it introduces our own session lifecycle and monitoring responsibilities.

### Background on reusing the Dota account

The original same-account question exposed a real hazard: vendored [`GameCoordinator::init_raw`](rust/vendor/steam-vent/src/game_coordinator/mod.rs), line 123, publishes a complete single-app played list. Initializing Deadlock on the same connection would remove Dota's presence unless refactored. Independent logins would introduce playing-session contention; Steam client documentation distinguishes multiple apps in one login from competing playing sessions. [SteamUser documentation](https://github.com/DoctorMcKay/node-steam-user#gamesplayedapps-force).

The Dota worker also connects before checking its work reservations, so a database lease alone would not solve that session issue. Separate accounts remove this work from the implementation plan. Do not investigate or promise simultaneous same-account hosting as a release requirement.

### Hosted HTTP service as an alternative

The current Deadlock API router exposes custom create, ready, unready, start, leave, and party-to-match-ID lookup. It does not expose public team assignment, kicking, invitations, settings mutation, full snapshot retrieval, or webhook re-registration. This is enough to investigate convenient joining-code creation, but not enough to claim the same admission control as Cama's Dota host. [Custom-match router](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/mod.rs).

Creation accepts `game_mode`, `server_region`, visibility, `disable_auto_ready`, and other game settings. It returns `party_id`, `party_code`, and a callback secret when configured. The provider bot moves to a spectator slot and normally readies itself. It leaves after fifteen minutes from creation; callback URL/secret storage expires after twenty minutes. Visibility defaults public, so explicitly set `is_publicly_visible: false` for an inhouse. There is no request idempotency token in the inspected create contract. [Create implementation](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/create.rs).

Use HTTP `game_mode: "normal"` or `"street_brawl"`. Internally these are 1 and 4; private-lobby match category is separately 2. Regions have their own Deadlock enum: use named values such as `us_west` and `us_east`, with an explicit adapter rather than Dota IDs. [Mode and region types](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/types.rs).

**Do not set `min_roster_size` to eight or twelve.** Valve uses that name for the minimum hero-selection roster. The public create request has no human `team_size` setting. Select the game mode and independently enforce Cama's exact human roster. Valve localization includes Normal/Street Brawl custom-lobby controls, spectators/unassigned players, and assignment/readiness messages, which supports a manual-hosting path as well. Actual slot layout still needs a live test. [Valve localization](https://github.com/SteamTracking/GameTracking-Deadlock/blob/master/game/citadel/resource/localization/citadel_main/citadel_main_english.txt).

The ready/unready endpoints change the provider bot's state; they do not remotely ready every human. Use `disable_auto_ready` during qualification and prove whether it provides an effective launch gate with the current client. Do not equate an accepted start request with a running match. [Ready implementation](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/ready.rs).

Public control-operation quotas are currently 10/hour per IP, 100/30 minutes per key, and 1,000/hour globally, with separate operation namespaces. Keys use `X-API-Key`; valid keys replace IP quotas, and emergency mode can require a key. A provider key is distinct from Statlocker access. Treat quotas and availability as operational dependencies, not a service guarantee. [Rate-limit client](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/services/rate_limiter/client.rs), [header extraction](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/services/rate_limiter/extractor.rs), [create quota declaration](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/create.rs).

### Observations and results

The API documents settings callbacks at `callback_url + "/settings"` containing a party snapshot, and a start callback at `callback_url` containing the match ID. A separate `GET /v1/matches/custom/{party_id}/match-id` provides a recovery route. [OpenAPI](https://api.deadlock-api.com/openapi.json). The lookup reads Redis and can return not-found; retention and webhook retry guarantees were not established. [Lookup implementation](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/custom/get.rs).

Callback handling requires an inbound endpoint reachable by the provider, which is an additional deployment requirement beyond an outbound Discord bot. Creation documents verification against `X-Callback-Secret`; verify delivery with actual fixtures. Reject unauthenticated/mismatched events, persist accepted events before acknowledging them, and reconcile duplicates or out-of-order snapshots. Never accept an arbitrary external callback as authorization to record a winner. A callback may race the create response that supplies its secret; queue or quarantine it until verification is possible.

For results, investigate `GET /v1/matches/{match_id}/metadata?is_custom=true`. The implementation checks stored metadata before Steam fallback. [Metadata source](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/metadata.rs). Custom salt retrieval explicitly lacks another source, and the upstream fallback is constrained to 3/hour/IP, 300/hour/key, and 1,500/hour globally in the inspected implementation. [Salt retrieval](https://github.com/deadlock-api/deadlock-api/blob/master/api/src/routes/v1/matches/salts.rs). Therefore use delayed, bounded polling with backoff, cache successful responses, and retain manual recording. Public availability of ordinary matches does not establish reliable coverage of private games.

There is no documented match-completion webhook in the inspected custom-lobby contract. A start callback, a disappearing party, a timeout, and unavailable metadata cannot be treated as a completed game. Brawl result parsing must use the authoritative final winner and validate side mapping; the round summary is supplementary.

### Durable hosting lifecycle

For either host adapter, proposed states are `pending -> economic_setup -> create_requested -> gathering -> betting_closed -> launch_requested -> running -> result_observed -> recorded -> settled`, plus `uncertain`, `expired`, `aborted`, and `manual_resolution` states. Keep the hosting attempt separate from the Discord queue and match. A user retry must attach to the existing attempt rather than manufacture another match.

Create the remote party only after the roster and mode are committed. The fifteen-minute limit applies only to the HTTP provider. Persist the request attempt before sending it, then persist IDs and any callback secret immediately after success. A lost response can leave a real remote lobby without a known party ID: reconcile the dedicated account's authoritative party state before recreating. An HTTP adapter without a snapshot/recovery route must surface recovery or explicit replacement as an organizer action.

When observations are fresh and identity-complete, verify selected accounts, sides, game mode, readiness, and region before requesting launch. If the provider cannot supply sufficient observation, the human organizer must own that verification; describe the feature as assisted hosting. The inspected API cannot promise an atomic roster-check-and-launch transaction, prevent a human from starting elsewhere, or eject unwanted players. These limits matter more than whether there is a `start` endpoint.

On restart, rehydrate the hosting attempt, match, claims, and captured events. Use party-to-match lookup where available. If the provider has lost its state or observations are ambiguous, retain the known local match and switch to manual resolution. Do not release a running match's player claims merely because the hosting bot's session expired.

### Protocol control and reuse boundaries

For full host control, tracked Deadlock protobufs define party create/join/invite/start/ready and actions including `SetMemberTeam`, `SetPlayerSlot`, `KickUser`, and `SetPrivateLobbyGameMode`. They also enumerate permission and invalid-state errors. [Client messages](https://github.com/SteamDatabase/Protobufs/blob/master/deadlock/citadel_gcmessages_client.proto). Shared objects contain party membership/readiness/slots and the lobby's match ID and server lifecycle state; the inspected lobby object does not directly report the winner. [Shared protocol](https://github.com/SteamDatabase/Protobufs/blob/master/deadlock/citadel_gcmessages_common.proto). Protocol definitions establish a possible implementation route, not successful current operations under a particular host account.

A direct Rust adapter needs Deadlock access on a host Steam account, session/authentication, the Deadlock handshake and current version handling, generated protocol types, shared-object snapshot/delta decoding, permissions, reconnection, durable orchestration, and live tests. Existing Steam auth is useful, but [`handshake.rs`](rust/crates/cama-steam/src/handshake.rs) and Dota lobby code cannot be switched by changing a single app ID. The [Go coordinator project](https://github.com/paralin/go-deadlock) is a protocol reference, not a drop-in Rust production implementation.

Keep hosting capabilities explicit: assisted HTTP hosting and full GC hosting should not pretend to implement the same unrestricted Dota host interface. The dedicated-account GC adapter is now the preferred path. HTTP remains a fallback/reference, and its hosted bot does not use our dedicated account.

### Live qualification before automation

Complete one real game in each format with human testers, then test recovery failures deliberately in disposable lobbies:

1. Verify selected ruleset, region, eight/twelve human slots, spectators, code joining, and private visibility.
2. Capture party/settings/start payloads and verify account IDs, sides, readiness, callback authentication, and large ID parsing.
3. Test extra players, wrong-side players, missing players, host permission errors, normal hero selection, ready/unready/start behavior, and persistent betting closure before launch.
4. For the dedicated host, test lost create/start responses, stale shared-object caches, reconnect, restart, unknown existing party, busy-account routing, and cleanup. If using HTTP, additionally test its fifteen-minute expiry and missed/duplicate/out-of-order callbacks.
5. Finish both modes and retrieve final result metadata. Prove private-game coverage and the exact whole-match Brawl winner mapping.
6. Verify that provider outage or missing results still permit manual recording without duplicate ratings or stranded claims.

Automatic result recording remains gated on this evidence. Bot-created lobbies can operate with audited manual results if final-result retrieval is not yet reliable; required betting still needs correct locks, payouts, refunds, and recovery in that mode.

## Betting: required first-release integration

Use the existing **guild-scoped JC wallet**, pool payout rules, supported leverage, participant restrictions, automatic liquidity, and opt-in investments. The betting event is one complete Deadlock match: in Brawl, settle the final match winner, never an individual round. Do not create a separate Deadlock currency or award Dota participation rewards merely to make betting work.

### Existing coupling and the implementation boundary

The current economy is reusable, but its match references are Dota-specific:

| Current behavior | Evidence | Deadlock work |
| --- | --- | --- |
| Placement resolves Dota pending-match JSON | [`betting_service.rs`](rust/crates/cama-db/src/betting_service.rs), around 371 and 2650 | Resolve a typed game-aware market and validate its committed roster, deadline, status, and completed setup. |
| Wallet debit uses a SQLite write transaction | Same file, around 2526 | Keep shared balance/debt checks and debit in one transaction across both games. |
| `bets.match_id` references Dota recorded matches | [`canonical_schema.sql`](rust/schema/canonical_schema.sql), around 50 | Never put a Deadlock numeric ID in this column. A collision could silently resolve to a real Dota match, especially with foreign keys disabled. |
| Commands offer Radiant/Dire and choose from Dota pending matches | [`betting_provider.rs`](rust/crates/cama-runtime/src/betting_provider.rs), around 1108 and 7650 | Show game, format, market, and game-appropriate side names; keep a stable internal side-one/side-two mapping. |
| Seed settlement reads Dota match rewards/participants | [`betting_service.rs`](rust/crates/cama-db/src/betting_service.rs), around 2993 | Supply a frozen market funding record and game-specific participants, not a fabricated Dota match. |
| Gambling statistics join bets to Dota matches | [`gambling_stats.rs`](rust/crates/cama-db/src/gambling_stats.rs), around 459 and 821 | Include both games deliberately in economic totals while keeping Dota competitive statistics scoped to Dota. |

Recommended approach: additive game-neutral market, wager, funding, and settlement records for Deadlock, with a typed `BettingMarketRef` carrying guild, game, immutable market ID, participants, sides, mode, and deadline. Extract shared payout calculations and transactional wallet helpers. Keep existing Dota rows behind a legacy adapter initially; migrating the entire Dota betting history is not a prerequisite. This adds an explicit compatibility boundary without maintaining a second copy of the economic rules.

Carry that namespace through every economic reference: ledger related IDs, automatic setup keys, tax records, Blood Pact deductions, settlement jobs, and refund/correction receipts. A new wager table alone does not prevent Deadlock market 1 from colliding with Dota pending or recorded match 1 in a downstream hook.

Reuse `/bet` routing where practical, with game-aware match selection/autocomplete and market-bound buttons. Do not require users to know which repository stores the match. When multiple markets exist, require an unambiguous selection; a naked local integer cannot identify both games. Every button must bind to the immutable market ID, and handlers must validate that the selected side belongs to that market. Resolve current in-game side labels from verified protocol/client evidence rather than assuming historical names are permanent.

Never-Dota users need wallet enrollment. Existing registration can require OpenDota/MMR input ([`registration_provider.rs`](rust/crates/cama-runtime/src/registration_provider.rs), around 1360 and 1406); extract an idempotent wallet/identity initialization path that does not grant Dota eligibility or invent a Dota rating. Audit Dota readers of `players` so a wallet-only row cannot enter a Dota shuffle. Existing users keep their balance, debt, and economic settings, with no duplicate initial grant.

### Preserve economic rules and define game scope

Placement currently restricts participating players to their own team, debits effective stake immediately, and supports leverage 1, 2, 3, 5, and mana-gated 10. Reuse the existing authorization, debt, and mana rules for eight or twelve participants; do not substitute a new balance check or simplified payout formula. For pool betting, preserve effective-stake weighting, seed distribution, per-user rounding, per-bet allocation, and the existing no-winning-stake policy. Verify these through shared engine tests, including tax/loan consequences where the current economic path applies. [Placement and payout implementation](rust/crates/cama-db/src/betting_service.rs), around 2523–2893; [leverage handling](rust/crates/cama-runtime/src/betting_provider.rs), around 3480.

Some economic hooks live outside core payout: match-loan repayment, bankruptcy-counter advancement, and Blood Pact profit deductions. Make their scope explicit. Recommended first-release policy: apply shared-wallet debt limits and applicable bet-profit deductions/garnishment/Blood Pacts to Deadlock profits, with separate durable receipts for effects outside the payout transaction. Preserve legacy match-count loan deadlines and penalty-counter progression as Dota-scoped until those contracts are deliberately extended; a shorter Brawl game should not silently advance an existing Dota match-based obligation or clear a penalty. Keep Dota low-priority progression and participation rewards unchanged. This is a proposed compatibility policy, not an existing game discriminator. [Runtime economic hooks](rust/crates/cama-runtime/src/match_provider.rs), around 1270, 4757, and 4828.

Automatic liquidity is part of parity: the existing shuffle sets up seed reservation, participant blinds, configured investments, and spectator balancing in order. Eligibility is captured before blinds; investment sizing uses post-blind balances. Preserve that ordering and give each operation a unique setup key so retrying cannot debit twice. Public manual bets open only after setup finishes. If setup fails partially, resume from durable receipts or compensate completed steps before aborting. [Shuffle setup](rust/crates/cama-runtime/src/match_provider/shuffle_setup.rs), around 394 and 474–564.

Existing investment preferences are guild/investor/target scoped without game identity. Introduce explicit Dota, Deadlock, or both-game scope; migrate existing preferences to Dota. Keep the existing per-target and aggregate percentage limits, and evaluate concurrent commitments against the shared wallet. Deadlock opt-in must be explicit rather than silently extending an old Dota preference. The same applies to automatic-bet settings whose current meaning is implicitly Dota. [Investment schema](rust/schema/canonical_schema.sql), table `autobet_investments`.

Funding needs game identity too. Dota seeds consume guild fund earmarks, including `nonprofit_fund.next_match_pot`, and have Dota-specific Open/LowSkill scheduling. A Deadlock market must not consume a pot advertised for the next Dota game or duplicate its daily allocation. Add per-game earmarks/reservations; any allocation from the common treasury must debit that treasury once. Initially support an explicitly configured Deadlock seed, including zero when unfunded, with transparent market display. Keep transfers, withheld amounts, rounding, refunds, and payouts reconcilable. [Dota seed reservation](rust/crates/cama-db/src/dota_bet_seed.rs), around 418 and 441.

### Locking, settlement, and recovery

Freeze mode, roster, sides, and funding terms **before the first automatic or manual wager**. Changing Brawl to Standard after automatic setup already requires cancelling/refunding that market and creating a new identity. Do not edit the winning proposition underneath existing wagers.

Give Deadlock its own betting-window configuration. A proposal is to publish teams and run the configurable window while players join; launch waits for valid readiness and the market's durable closure. Persist closure before sending the launch request. Earliest observed gameplay also closes betting, covering an unexpected human start. Never reopen on reconnect, uncertain start response, or an empty party snapshot. Manual-host fallback needs a conservative deadline and an explicit close-before-start organizer action. If live testing cannot establish reliable launch gating or prompt start detection, automatic hosting must not promise an enforceable pregame market without organizer verification. The existing Dota timestamp-based `bet_lock_seconds` behavior is not sufficient evidence for Deadlock's gate. [Existing lock setup](rust/crates/cama-runtime/src/match_provider/shuffle_setup.rs), around 387.

Result recording and settlement should be a durable workflow: `recorded -> settlement_pending -> settled`. Commit the verified result, rating history, and settlement job together; settle under a write transaction that validates winner/market identity and stores a unique receipt. Retry after a crash between those commits. An accepted result, restart, or Discord publication retry must never pay twice. The current Dota recorder and economy already cross separate transactions, so simply calling its settlement function from a new command is not enough. [Runtime finalization](rust/crates/cama-runtime/src/match_provider.rs), around 4710; [existing settlement](rust/crates/cama-db/src/betting_service.rs), around 1460 and 1735.

Persist the applicable settlement policy with the accepted result: taxable users, bankruptcy basis/policy, payout multiplier, fees, garnishment, and the applicability of shared loan/Blood Pact effects. Market terms are already frozen before betting; result-time economic inputs must also survive retries. A delayed worker must not recompute obligations under a later configuration or temporary economic effect. The existing Dota recorder saves `recording_policy` for this reason; adapt the relevant economic policy explicitly without invoking unrelated Dota reward hooks. [Policy snapshot](rust/crates/cama-runtime/src/match_provider.rs), around 4580–4607.

Abort is also a transaction with an idempotent receipt: reject a normally recorded/settled match, refund the debited effective stakes, release reserved funding according to its source, archive the reason and actor, and close the market. Do not inherit Dota exclusion/participation mutations. Missing metadata is an unresolved game, not an automatic refund trigger. Rating correction and money correction are separate: a paid winner correction needs an audited compensating economic ledger and an explicit debt/fee reversal policy; it must not run ordinary settlement a second time. [Existing abort semantics](rust/crates/cama-db/src/betting_service.rs), around 1284.

Display market state, pool totals, deadline, format, and eventual payout/refund status in the match thread. Keep manual resolution available to authorized organizers/admins, with the recorded evidence and actor visible. These recovery paths are release requirements, not postlaunch administration work.

## Persistence and concurrency design

Suggested table names below are illustrative. The important invariants are identity, separation from old Dota readers, and atomic lifecycle transitions.

| Record | Minimum durable fields |
| --- | --- |
| Game enrollment | Guild, Discord user, game, selected linked Steam account, enrollment state, timestamps. |
| External rating snapshot | Provider, account, source mode/season/era, raw value and badge, source/fetch time, calibration/coverage when available, mapped value, uncertainty, transform version, fallback reason. |
| Local game rating | Guild/user/game/pool key, rating/RD/volatility, games, wins/losses, last game, seed snapshot, policy version. |
| Deadlock queue | Guild/queue ID, format ID/version, revision, organizer, region, capacity, lifecycle, thread/message IDs. |
| Queue membership and readiness | Queue/user key, join time, waiting priority, readiness generation, confirmed format/revision. |
| Deadlock match and participants | Local ID, source queue, immutable format/region/rosters, selected Steam accounts, rating snapshots, lifecycle, result provenance, external match ID when available. |
| Rating history | Match/user/pool identity, before/after state, policy version and result revision. |
| Shared player claim | Unique guild/user key, owner game/type/session, lifecycle metadata. |
| Hosting attempt and publication jobs | Durable request/attempt identity, session IDs, external state, retry metadata, publication status; sensitive credentials redacted. |
| Betting market and wagers | Globally unambiguous market identity, guild/game/match reference, immutable sides/roster/format, window/status, setup version, bettor, nominal/effective stake, leverage, creation/idempotency key. |
| Funding and economic receipts | Funding source and game earmark, reserved amount, automatic setup operations, wallet movements, settlement/refund/correction receipt, actor/reason and result revision. |
| Settlement jobs and preferences | Result-to-market job with retry/status and frozen settlement policy; game scope on automatic betting/investments; per-game funding configuration. |

Retain the existing global `player_steam_ids` ownership table ([schema](rust/schema/canonical_schema.sql), line 1308), including its unique Steam account constraint. Add game-independent identity linking and wallet initialization without requiring Dota enrollment. Deadlock enrollment should not silently change the user's global primary Steam account; store its chosen account in the game enrollment and freeze it per match. Multiple linked accounts are not multiple slots for one Discord participant.

Use typed Steam account IDs and SteamID64 conversions. Providers describe their numeric account IDs as SteamID3; this is the account-number component, not a full textual `[U:1:...]` value. Do not pass SteamID64 where an account number is expected. Preserve full-width external party/match IDs through JSON and storage.

Extend [`schema_manager.rs`](rust/crates/cama-db/src/schema_manager.rs), the canonical schema, and [`expected_migrations.txt`](rust/schema/expected_migrations.txt). Production repositories must not introduce DDL. [`open_runtime_connection`](rust/crates/cama-db/src/lib.rs), line 193, uses foreign keys OFF, so new `REFERENCES` clauses alone do not enforce integrity. Add transactional checks, appropriate uniqueness/check constraints, and regression tests without globally changing that connection policy as a side effect.

### Prevent the same player being committed to Dota and Deadlock

Today, in-flight reservations belong to the Dota `LobbyService` ([lines 754–801](rust/crates/cama-app/src/lobby_service.rs)). Dota pending-match creation transactionally inspects Dota pending rows ([lines 320–358](rust/crates/cama-db/src/match_runtime.rs)); draft finalization has its own insertion path ([lines 397–430](rust/crates/cama-db/src/draft_finalization.rs)). A separate Deadlock mutex plus a preflight query cannot prevent both games committing the same person concurrently.

Add a shared claim repository, uniquely keyed by `(guild_id, discord_id)`. Each game acquires claims atomically with pending creation. Dota drafts claim at draft start and transfer ownership at finalization. In-memory reservations can provide fast feedback, but durable uniqueness is the authority. Claim only selected players, not the bench.

Release only the exact owner's claims after abort or recorded completion. Cover all draft/pending deletion and recovery paths; a generic “delete user's claim” can accidentally free a newer match. Backfill/reconcile existing Dota pending matches and active drafts before enabling Deadlock. Preserve conflicts for resolution instead of choosing one owner and deleting another's data.

Allow gathering in multiple queues if desired, but remove selected players or mark them unavailable in other queues after commitment. Keep guild scope consistent with current behavior; global exclusivity across unrelated guilds is a separate product choice.

### Recording and corrections

The Deadlock recorder validates a committed match, exact distinct rosters, format, and winner; inserts the result and history; updates only the corresponding rating pool; schedules durable settlement; and releases owned player claims in one transaction. Host-account release separately requires safe session cleanup. A unique match/result identity makes duplicate requests idempotent. Conflicting winners require an explicit correction flow rather than a second update. Calculate from current rating rows under the write transaction, or validate rating revisions before committing an externally calculated update. Player claims alone do not serialize concurrent rating overrides and correction replay.

Manual recording should use an organizer/admin policy or a deliberately specified participant confirmation rule. Do not inherit fixed Dota vote counts without checking their meaning for eight versus twelve players. Initially, organizer/admin recording with a visible actor and audit record is a smaller implementation than a new voting system.

An aborted unplayed match produces no rated result and follows the market refund procedure above. For a mistaken recorded result, retain an audit revision and deterministically replay subsequent results in that affected pool when needed; subtracting a stored delta is insufficient once later Glicko updates depend on the old state. Store admin overrides and reseeding decisions as durable ordered rating events, including actor and reason, so replay does not silently erase them. Reconcile paid wagers through audited compensating transactions separately from rating replay. If automated correction is deferred, document an admin repair procedure before release.

External outcomes must validate game mode, external match identity, frozen participating accounts, team mapping, and final status. An abandoned, cancelled, incomplete, mismatched, or unknown result remains unresolved for manual handling. An HTTP 200 or a match ID alone is not sufficient evidence.

## Implementation sequence and effort

These are planning estimates for an engineer familiar with this Rust repository, not measured delivery commitments. External access approval and finding eight/twelve testers add elapsed time independently.

| Work package | Deliverable | Rough effort |
| --- | --- | --- |
| Provider and dedicated-account spike | Confirm rating fields/modes; compile protocol adapter; authenticate, create both formats, qualify launch control and final results. | 2–4 engineering days, plus access/tester wait |
| Additive persistence and identity | Enrollment, wallet-only users, rating provenance, queues/matches, migrations, shared claims and Dota lifecycle integration. | 3–5 days |
| Discord queue and balancing | Brawl default/Standard override, readiness revisions, fair selection, 4v4/6v6 teams, publication/recovery. | 3–5 days |
| Ratings and manual results | Provider adapter/fallback, versioned seeds, local updates, audited record/abort/history and correction procedure. | 3–5 days |
| Core betting integration | Typed markets, shared wallet engine, commands, locks, payout/refund receipts, durable settlement and reports. | 5–9 days |
| Automatic economic setup | Game-scoped seeds, blinds, investments, spectator liquidity, partial-setup recovery. | 3–6 days |
| Dedicated Steam host | Party/cache adapter, account worker/reservation, versioning, launch gate, reconciliation, cleanup and qualified result retrieval. | 5–10 days after a successful spike |
| Integrated regression and pilot fixes | Wallet races, concurrency, restart, migration, outages, both formats, and Dota regression verification. | 3–5 days |

The packages sum to roughly **27–49 engineering days**, or about **6–10 working weeks for one engineer**, before external waiting time. This is a broad planning range for the requested integrated release, including betting and a dedicated host; some work overlaps. The host/result portion has the lowest confidence and must be re-estimated after the spike. Protocol incompatibility or inaccessible private results may change the automation scope materially. A manual-host fallback can preserve play and betting while hosting is repaired, but betting is not a deferred add-on in this estimate.

Keep PRs reviewable: run the host/provider spike first; then introduce game/format primitives, storage and economic contracts; identity/ratings and shared claims; queue UI/balancing; betting/setup/settlement; and the host worker and recovery integration. Hosting development can proceed independently once contracts stabilize. Keep the feature disabled until migrations and claim reconciliation succeed. Enable it in one guild/channel only when required betting and recovery work, with explicit manual host/result fallbacks.

## Acceptance and validation

The research change only adds this document; no production code or database was modified and no Rust test run is represented as implementation validation. The eventual change needs the repository's required Rust gates plus focused tests at the following boundaries.

| Boundary | Required cases |
| --- | --- |
| Migration | Fresh and upgraded schema, repeat migration, existing Dota data preserved, unknown format rejected, claim backfill conflicts surfaced. |
| Identity | Never-Dota player; existing Dota user; alternate account; conflicting global Steam ownership; account change without rating reset; signed ID boundaries; independent guilds. |
| Rating adapter | Success, missing/private, stale, uncalibrated, malformed/NaN/infinite/out-of-range, timeout, 401/403/429/5xx, reordered/missing batch entries, transform/season version. |
| Enrollment | Concurrent registration initializes once; provider calls occur outside write locks; established local rating never reset. |
| Format | Exact four/six per side; eight/twelve required across display/readiness/shuffle/record; oversubscription and bench fairness. |
| Mode switch | Omitted format always defaults Brawl; Standard requires twelve; old button/readycheck sweep, late provider response, switch/shuffle race; no mutation on insufficient players; wagered market changes require refund/new identity. |
| Claims | Dota shuffle versus Deadlock shuffle; Dota draft versus Deadlock; restart during each; release only current owner; selected players versus bench. |
| Recording | Duplicate and conflicting winner, abort race, atomic rollback, restart after commit, override/correction versus recording, replay preserving overrides, whole-game Brawl result. |
| Shared wallet | Simultaneous Dota/Deadlock bets cannot overspend; all supported leverage/debt/mana policies and participant own-team rules; wallet-only enrollment grants once; currency/fee/debt changes reconcile to explicit economic operations. |
| Markets and settlement | Colliding game-local IDs including ledger/tax/hook references, wrong guild/side, stale buttons, lock-versus-bet race, launch without reopening, duplicate/conflicting results, crash after record/before payout, policy/config change before retry, zero winning stakes, exact rounding, refund-versus-settlement race, refund retry, paid correction, post-payout hook retries. |
| Automatic betting | Ordered seed/blinds/investments/liquidity, partial setup retries, scope migration, concurrent budget use, no Dota pot consumption, abort returns source funding, reports include both games correctly. |
| Dota isolation | Deadlock cannot change Dota ratings, roles, wins, pairings, exclusions, streaks, enrichment, bet records, referrals, moderation counters, or pets through accidental hooks. Shared wallet/debt/economic statistics change only through intended betting operations. |
| Dedicated Steam account | Separate credentials/token directories and verified account identity; Dota host stays connected; one reserved Deadlock host; reconnect/cache hydration; lost create/start reply; fresh roster check; both modes; cleanup and result evidence. |
| Discord | Acknowledgment before slow work, cold name cache, absent Dota player row, deleted thread/message, publication failure, duplicate click, command registry limits. |
| Runtime | Durable queue/match/claims recover; outbox retries without duplicate matches; provider outage does not strand normal manual play. |

Use deterministic fake provider ports, captured authorized HTTP fixtures, temporary SQLite databases, injected clocks and entropy. Follow the real compilation ownership of `cama-domain`, `cama-db-core`/`cama-db-match`, application slices, and `cama-runtime-engine`; a passing facade-only test is not evidence every owner was exercised.

Before shipping, run `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`, the workspace Clippy gate, and the full workspace test gate from `CLAUDE.md`. A live pilot then confirms the parts mocks cannot establish: player joining, real mode selection, external rating semantics, external match visibility, and service recovery.

## Decisions and unresolved dependencies

The implementation target is one queue, Street Brawl by default, an explicit Standard option on shuffle, separate local rating pools, fair roster selection, required shared-wallet betting, and a separate Steam host account. No Deadlock hero draft, player draft, or captain draft is included. Use audited organizer/admin results and aborts as recovery paths. Share player claims and economic primitives deliberately; keep Dota rating and reward hooks out of Deadlock recording.

The remaining external questions are concrete:

- Statlocker: approve access and confirm which mode/era `ppScore` represents; whether Brawl ratings are available through the supported API; null/calibration/freshness semantics; and the intended current scale. Obtain permissioned fixtures rather than scraping.
- Dedicated Steam host: confirm Deadlock entitlement, crate/build compatibility, current coordinator version, spectator permissions, side/slot mapping, launch control, reconnect behavior, and private final-result access for both modes. These are live qualification tasks, not reasons to reuse the Dota account.
- HTTP alternative, only if selected: verify real settings/callback auth, human/host controls, expiration, recovery retention, and private result coverage. This is a third-party host, not an API for logging in our dedicated account.
- Rating policy: validate a fixed initial normalization using representative authorized player data; retain an explicit provisional fallback when evidence is inadequate.
- Economy configuration: specify the Deadlock betting-window duration and treasury/seed allocation; migrate legacy investments as Dota-only and provide explicit Deadlock opt-in. Keep payout, leverage, debt, and rounding rules consistent with the existing shared economy, with the proposed Dota scope retained for legacy match-count obligations/counters.

The repository and public protocol provide a credible Rust implementation route, but no source establishes turnkey Dota parity. A successful dedicated-account, two-mode live spike is the key evidence needed before committing to automatic hosting and result settlement. The rest of the implementation can proceed against explicit host and result interfaces, with betting included from the start.
