# Deadlock matchmaking

Deadlock uses a dedicated **`#deadlock-mm`** text channel and an attached public thread for each match. Street Brawl 4v4 is the default. Standard 6v6 is an explicit per-shuffle choice. There are no Deadlock drafting or captain commands.

Manual in-game hosting and manual result recording are the default for the initial rollout. Players create the private lobby, select the shuffled format, and assign the published teams; the organizer or an admin records the final winner. The bot handles the Discord queue, ratings, match history, and betting. Automatic Steam hosting and automatic result recording stay disabled unless explicitly enabled later, after live qualification.

## Enable a guild

Give the bot View Channel, Send Messages, Embed Links, Read Message History, Create Public Threads, and Send Messages in Threads. Manage Threads is useful for recovering archived threads. Enable the guild:

```dotenv
DEADLOCK_ENABLED=true
DEADLOCK_GUILD_IDS=YOUR_GUILD_ID
# Optional: direct the lobby to an existing text channel.
# DEADLOCK_CHANNEL_ID=YOUR_CHANNEL_ID
DEADLOCK_BET_WINDOW_SECONDS=180
DEADLOCK_BET_SEED_AMOUNT=0
DEADLOCK_STEAM_ENABLED=false
DEADLOCK_STEAM_AUTO_RECORD=false
```

`DEADLOCK_CHANNEL_ID` selects an existing text channel for a single enabled guild. When unset or blank, the bot uses the existing text channel named `deadlock-mm`. If that channel is absent, it reports the missing destination; it never creates channels. A configured ID must refer to a text channel in that guild; an invalid override is reported and never silently redirected.

For multiple guilds, use optional `DEADLOCK_CHANNELS=guild_id:channel_id,guild_id:channel_id` overrides; guilds listed in `DEADLOCK_GUILD_IDS` without an override use `deadlock-mm`. Unconfigured guilds are rejected. Queue, registration, and rating commands can run anywhere in the enabled guild and link back to the lobby. Matchmaking and betting actions run in the lobby channel or its threads; existing matches retain their original channels and threads after a destination change.

Each guild has one queue embed whose message ID is persisted. Joins, departures, readiness, rating changes, and nickname changes edit that message, with private command confirmations. Startup and periodic recovery reuse it, including recovery from an interrupted send. A definitively deleted message is replaced; a Discord error waits for recovery. The first reconciliation adopts an existing bot-owned queue panel and removes duplicate queue panels found in the most recent 500 channel messages. Match starters, betting messages, and player posts are preserved.

The feature is disabled unless explicitly enabled. Startup validates the configuration. Existing Dota commands and hosting continue independently. Rust schema initialization adds the Deadlock tables and migration ledger entries; do not manually create tables or apply DDL to a running production database.

## Player flow

1. `/deadlock register steam:<SteamID64 or account number>` links the account and imports an initial prior. Existing globally linked Steam ownership is respected. Re-registering cannot reset ratings or grant another signup balance. New wallets receive the existing 3-JC signup amount once; existing wallets and debts are preserved. Deadlock registration does not enroll the player in Dota.
2. `/deadlock join` enters the persistent FIFO queue. The channel message includes join, leave, and readiness buttons. `/deadlock ready` confirms Brawl availability for ten minutes; `/deadlock ready format:standard` confirms Standard instead.
3. `/shuffle lobby:deadlock` selects eight ready players and balances 4v4. `/shuffle lobby:deadlock format:standard` selects twelve and balances 6v6. `/deadlock shuffle` is an equivalent dedicated command. Standard never falls back silently to Brawl. Unselected players retain their queue position.
4. The bot commits the roster and initial economic setup before posting the match. Each match has a stable number and a thread under `#deadlock-mm`. Players create the private in-game lobby in the selected format, share its join code in that thread, and use the published teams. Silent participant mentions subscribe players without noisy member-add events. A host reservation and automatic join code are only available when Steam hosting is explicitly enabled.
5. `/bet game:deadlock match:<number> team:team1 amount:<JC>` or `/deadlock bet` places a wager. The match buttons open a stake/leverage form. Use `/deadlock bets`, `/deadlock mybets`, and `/balance` to inspect exposure.
6. For manual hosting, the organizer must use `/deadlock close match:<number>` **before gameplay starts**. A host with qualified launch control closes betting durably before requesting launch. A reconnect never reopens a market.
7. The organizer or an admin records the whole-game winner with `/deadlock record match:<number> winner:team1`. Include `external_match` when known. For Brawl, record the final match winner, not a round. Recording requires a closed market. `/deadlock abort match:<number> reason:<reason>` voids an unresolved match and refunds stakes. Once the Steam host has admitted launch, ordinary abort is blocked: an uncertain start requires reviewed resolution before any refund.

`/deadlock lobby`, `/deadlock match`, `/deadlock rating`, and `/deadlock history` provide current state. The original shuffle requester is the organizer; configured bot admins and members with Administrator or Manage Server can resolve matches. In-game side assignment uses the verified host adapter's mapping; displayed Team 1/Team 2 identify the immutable Cama rosters.

Readiness is specific to the requested format. Queue participation alone is not readiness, and twelve queued players do not change the default mode. Dota pending-match and durable draft membership exclude overlapping Deadlock commitments in both directions.

## Ratings

Deadlock API supplies all external rating data. Basic lookups need no API key. An optional key can raise provider request limits:

```dotenv
DEADLOCK_API_KEY=...
```

Statlocker is used only for generated profile links: `https://statlocker.gg/profile/<Steam32 account ID>`. Registration includes the player's profile link; lobby and match rosters link readable names to their profiles. SteamID64 input is converted to an unsigned 32-bit account number. No Statlocker API request, API key, or confirmation flag is used.

The importer requests the current Valve ranked badge through Deadlock API and retains its provenance. Missing/private/unranked/provider failures use an explicit neutral prior. Registration shows a simple default-rating message, while logs retain diagnostic details such as HTTP errors and absent ranked badges, without exposing credentials. Requests have bounded timeouts, bounded response bodies, no redirects, and a short bounded cache. Keys are redacted and requests do not hold SQLite write locks.

The importer maps the supplied ranked badge to the local OpenSkill scale. It does not divide Valve MMR by four. The Brawl prior shrinks the Standard estimate's deviation from the neutral rating by 75%. Lobby embeds show that format's local rating after each player's name. Existing saved rating sources remain readable and existing imported or played ratings are preserved.

If an initial import failed, repeat `/deadlock register` with the same Steam account after correcting provider configuration or availability. Only an untouched neutral prior with zero games and zero rating revisions can be filled. A committed match blocks this update, and existing imported or played ratings are preserved. Retrying grants no new signup funds.

Local ratings use the existing OpenSkill team update with independent Brawl and Standard pools. The initial badge ordinal maps to a **weak, provisional** prior on the OpenSkill scale; it is not claimed to be a statistically calibrated conversion. Brawl receives a strongly shrunk Standard prior until a supported Brawl import contract is available. High uncertainty lets local games move that initial estimate. Every enrollment stores the source, raw value, transformation label, and provenance. Established ratings are never overwritten by refresh or re-registration.

## Betting and funding

Deadlock shares the existing guild JC wallet, debt ceiling, leverage tiers, applicable mana effects, profit deductions, and pool payout algorithm. Participants can bet only on their own side. Dota wagers and numeric match IDs are never reused for Deadlock. Financial operation keys, tax receipts, settlement, and Blood Pact jobs carry Deadlock market identity.

Participant blinds use the configured `AUTO_BLIND_*` policy. Existing Dota investments are not silently opted into Deadlock. `/deadlock invest player:<user> percentage:<1–10> direction:long|short` adds a Deadlock position; zero removes it. The combined investment preference cap remains 50% across games. `/deadlock liquidity percentage:<0–10>` controls optional Deadlock spectator liquidity. Setup reserves funding, applies blinds, then investments and spectator liquidity atomically before manual bets open.

`/deadlock fund amount:<JC>` transfers an explicit contribution from the caller's wallet to the Deadlock seed earmark. `DEADLOCK_BET_SEED_AMOUNT` specifies the requested per-match seed; its default is zero. No Dota daily allocation or next-match pot is consumed. An unfunded requested seed prevents market setup rather than minting currency. Add funding and inspect the saved match to retry, or abort it.

The wallet already reflects debited wagers. `/balance` lists Dota and Deadlock match stakes separately from prediction-contract positions; stakes are not counted as guaranteed assets. Combined gambling history includes both games; Dota competitive records remain separate.

Recorded results and settlement are durable independent steps. Retries do not apply ratings, stake debits, credits, refunds, or economic hooks twice. Terms are frozen in the saved match/market; individual bankruptcy basis is frozen on the bettor's first wager. Legacy Dota match-count penalties and loan deadlines are not advanced by a Brawl game. Applicable shared profit deductions remain in effect.

Changing a committed roster, side, or format requires an explicit abort/refund and a new match. Conflicting or corrected paid results are deliberately rejected; they require reviewed administrative repair of the audit/rating history and compensating economic entries, not another `/record` or ordinary abort. Never edit a settled winner in place or delete economic receipts.

## Dedicated Steam account

This is an optional future opt-in. Leave `DEADLOCK_STEAM_ENABLED=false` and `DEADLOCK_STEAM_AUTO_RECORD=false` for the initial manual-host rollout; no Steam account credentials are required for that flow. The configuration below enables hosting and should only be used when ready to qualify automation.

The account must have Deadlock access and must differ from the Dota host account. Its session and machine-token files must also be separate. Configure a single active Deadlock host:

```dotenv
DEADLOCK_STEAM_ENABLED=true
DEADLOCK_STEAM_ACCOUNT_ID=STEAM32_ACCOUNT_NUMBER
DEADLOCK_STEAM_USERNAME=...
DEADLOCK_STEAM_SESSION_PATH=data/steam/deadlock/session.json
# Optional authentication bootstrap values, handled as secrets:
DEADLOCK_STEAM_PASSWORD=...
DEADLOCK_STEAM_GUARD_CODE=...
# Or an authenticator shared secret, if used by the account:
DEADLOCK_STEAM_SHARED_SECRET=...
# Optional explicit machine-token file; default is beside session.json:
DEADLOCK_STEAM_MACHINE_TOKEN_PATH=data/steam/deadlock/machine_tokens.json
```

Coordinator configuration also requires **qualified current values**, not copied test fixture numbers:

```dotenv
DEADLOCK_STEAM_PARTY_SO_TYPE=QUALIFIED_PARTY_SHARED_OBJECT_TYPE
DEADLOCK_STEAM_LOBBY_SO_TYPE=QUALIFIED_LOBBY_SHARED_OBJECT_TYPE
DEADLOCK_STEAM_SERVER_REGION=QUALIFIED_DEADLOCK_REGION
DEADLOCK_STEAM_DATACENTER_CODES=QUALIFIED_DATACENTER_CODES
DEADLOCK_STEAM_PING_TIMES=MATCHING_MEASURED_PING_VALUES
DEADLOCK_STEAM_AUTO_RECORD=false
```

Shared-object type IDs are not established by the inspected public protobuf schema. They are explicit configuration rather than guessed Dota constants. Datacenter/ping lists must be nonempty and have matching lengths. Do not supply fictional ping data. The worker retrieves the accepted client version and uses private Normal or StreetBrawl settings, with the host in a spectator slot.

After supplying the dedicated account ID and authentication configuration, bootstrap authentication interactively. Login does not require the enabled flag, cache type IDs, region, or ping configuration:

```bash
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- deadlock-steam-login
```

`deadlock-steam-probe` uses the saved account session to report coordinator welcome cache type IDs, object counts, byte sizes, and candidate private-party/lobby shapes. It does not print raw objects, player IDs, join codes, or credentials. Use disposable lobby qualification to confirm candidate types; an empty cache or a shape match alone is not proof of the required type. Stop the running host before either operator command; both take the same account process lock.

Do not print, commit, or copy session credentials into application logs. The worker has a process lock and checks the authenticated account identity. A durable host reservation is made in the same transaction as the match. If the host is busy, that new match receives a permanent manual-host fallback rather than being adopted later when the account frees up.

The worker waits for saved channel/thread publication, creates the private party, observes exact membership/team/readiness, and keeps its spectator unready until betting closes. It saves launch intent before readying the host, because readiness itself may trigger allocation. An explicit start has its own durable one-shot marker. Lost create/ready/start replies retain the reservation for reconciliation or manual review. An unrelated party is not adopted or destroyed. Cleanup releases only the owned session after authoritative removal. Uncertain host state never reopens betting or authorizes a winner.

### Automatic result qualification

The transport can retrieve the metadata locator and decode bounded bz2/zstd metadata. The result validator requires the exact external match identity, private category, selected mode, frozen accounts and sides, plausible start time, and an explicit scored whole-match outcome. It rejects a round winner, missing/default winner, mismatched roster, and absent completion. Downloads use a fixed numeric Valve CDN URL over HTTPS with redirects disabled. If the CDN/result is unavailable, manual recording remains available.

After completing a live test in **both** modes and testing restart/reconnect recovery, automatic recording can be enabled with:

```dotenv
DEADLOCK_STEAM_AUTO_RECORD=true
DEADLOCK_STEAM_RESULT_ACTOR_ID=DISCORD_BOT_USER_ID
```

The actor is stored in the audit trail. Live qualification has not been performed by the implementation's offline test suite. In particular, current shared-object type IDs, server/datacenter values, spectator start permissions, and private metadata availability must be verified on the actual account.

## Recovery and operations

- The queue and readiness timestamps survive restart. Expired readiness must be renewed.
- The recovery worker resumes saved economic setup, settlement/refunds, Blood Pact jobs, and match/thread publication. An unknown send result is reconciled by stable delivery keys. It never creates another local match to retry a Discord post.
- `/deadlock match` can also retry saved economic setup. A failed setup retains the frozen roster and terms until recovered or aborted.
- Host errors appear as review-required state. Do not run another client with the same account to force progress while the worker is active. Stop the worker before investigating credentials or an unknown party.
- A missing result is not an automatic refund. The organizer/admin resolves the actual game with evidence or explicitly voids it with a reason.
- Keep the configured channel stable until its matches complete. Restoring deleted publication destinations requires an operator decision; monetary receipts remain authoritative independently of Discord messages.

The [research report](DEADLOCK_LOBBY_RESEARCH.md) records the source investigation and broader design considerations. This document describes the implemented command/configuration surface and the remaining live qualification boundary.
