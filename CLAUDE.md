# CLAUDE.md

Rust is the sole implementation language.

## Git

- Before pushing, run the Rust formatting, lint, and test gates below.

## Project

Cama Balanced Shuffle is a Discord bot for Dota 2 inhouse leagues. It provides balanced shuffling, captain drafts, rating systems, registration and OpenDota integration, match recording and enrichment, betting and prediction markets, the Jopacoin economy, Dig, pets, Mafia, duels, mana, trivia, Wrapped, image rendering, reminders, moderation, and optional AI flavor/SQL features.

Production behavior lives in `rust/`. Historical implementations and contracts can be recovered from Git history when needed; keep all current behavior and tests in Rust.

## Workspace

The Rust workspace uses edition 2024 and Rust 1.94:

- `rust/crates/cama-domain`: transport- and storage-independent domain policies and models.
- `rust/crates/cama-db-core`: Rust-owned SQLite initialization, migrations, integrity checks, and shared connection policy.
- `rust/crates/cama-db-{dig,economy,gameplay,match,platform}`: independently compiled repository slices.
- `rust/crates/cama-db`: compatibility facade over the database foundation and repository slices.
- `rust/crates/cama-app`: application services and orchestration behind typed persistence, clock, randomness, AI, and Discord ports.
- `rust/crates/cama-app-{dig,gameplay,match,platform}`: independent application slices re-exported by `cama-app`.
- `rust/crates/cama-runtime-core`: Serenity-independent runtime contracts and configuration shared by provider crates.
- `rust/crates/cama-runtime-commands`: independent leaf command providers compiled outside the runtime monolith.
- `rust/crates/cama-runtime-engine`: production Tokio/Serenity adapters, coupled providers and workers, gateway recovery, and health.
- `rust/crates/cama-runtime`: compatibility facade and production composition root over the independently compiled runtime crates.

Repository-wide architecture audits run in the `cama-runtime-engine` match
test shard; there is no separate `xtask` crate or cross-language parity gate.

Dependency direction is Domain → Database/Application → Runtime. Keep domain logic independent of Serenity and concrete storage. Keep SQLite access in `cama-db` and compose production adapters in `cama-runtime`.

## Commands

Run from the repository root:

```bash
# Format
cargo fmt --manifest-path rust/Cargo.toml --all -- --check

# Lint
cargo clippy --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features -- -D warnings

# Test the complete Rust workspace
cargo test --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features

# Test one implementation crate or one contract
cargo test --locked --manifest-path rust/Cargo.toml -p cama-runtime-engine

# Compile without running tests
cargo test --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features --no-run

# Run the production runtime; DB_PATH defaults to cama_shuffle.db
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- serve

# Read-only operational checks
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- db-check --db-path /path/to/cama_shuffle.db
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- inventory
```

Rust tests are the default and required validation.

## Architecture and conventions

### Discord providers

- Define slash-command and component schemas with `CommandSpec`, `CommandOptionSpec`, and `ComponentRoute` in `cama-runtime`.
- Implement behavior through `InteractionHandler` and `InteractionResponder`; keep Discord payload construction typed.
- Discord allows one initial interaction callback. Slow component and modal-submit routes use the runtime's automatic acknowledgement coordinator. Buttons that must open a modal return `InteractionAcknowledgementPolicy::Modal`, open the modal immediately, and move database/network validation to modal submission.
- After a deferred component update, edit the original response or send an appropriate follow-up. Never perform database, network, lock-wait, or image-render work before a modal is opened or an interaction is acknowledged.
- Preserve the 100 top-level Discord command limit and prefer subcommands.
- Always show players their live Discord display name (nickname, falling back to global display name), never their Discord username or a stale registered/DB name. Resolve it through the existing live-lookup helper (e.g. `render_player_name`/`resolve_player_name` in `lobby_provider.rs`) rather than a `player.name`/registration-record field, which can be a raw username or out of date. This has regressed more than once — check it explicitly when writing or reviewing any player-facing message.
- To silently subscribe a user to a thread (so they see new activity without opening it manually), post a message that `@mentions` them with `DiscordAllowedMentions::None` (`DiscordMessage::silent`, or an equivalent explicit user allowlist) rather than calling the thread-member API (`add_thread_member`) directly. Discord treats a mention in message content as an organic join and adds no extra text; the explicit thread-member API always posts its own unsuppressable "X added Y to the thread" system message on top of anything the bot sends, which reads as a confusing duplicate. `allowed_mentions: None` only silences the ping that the mention(s) in that message would otherwise trigger — it is per-message and per-mention, not a broader notification switch: the mention still renders as a clickable name pill, Discord resolves it client-side to the user's current live display name regardless of what text the bot supplies elsewhere, and it has no effect on any other message's mentions or on non-mention notification sources (e.g. a channel's general unread/activity indicators).

### Guild isolation

- Persistent guild data is scoped by `guild_id`; most player state uses `(discord_id, guild_id)` identity.
- Discord IDs are converted to signed `i64` before SQLite access and must fail closed if out of range.
- `None` remains meaningful for DM/global behavior. Do not silently borrow data across guilds.
- Steam/account-link identity is intentionally global where the repository contract says so.

### Database

- `cama-db` owns schema initialization and ongoing migrations.
- Preserve SQLite WAL mode, the five-second busy timeout, signed IDs, the existing migration ledger, and repository-specific coercion behavior.
- Use `BEGIN IMMEDIATE` or an existing atomic repository operation for economic, match, voting, settlement, and other race-sensitive mutations.
- Do not add DDL inside repositories. Extend the canonical schema/migration manager and add Rust migration and repository tests.


### Application services

- Put reusable policy in `cama-domain` and orchestration in `cama-app`.
- Inject repositories and external capabilities through typed ports rather than reaching into the runtime from application logic.
- Compose services/providers/workers in `cama-runtime/src/main.rs` and keep restart recovery idempotent.
- Keep blocking SQLite, image rendering, and other CPU/blocking work off Tokio workers with the existing blocking helpers or `spawn_blocking`.

### Configuration

- Runtime configuration is parsed by the Rust application configuration modules and environment lookup.
- Secrets must stay redacted in diagnostics and must not be written into tests, fixtures, or logs.
- See `rust/README.md` for deployment, health, database-copy, and operational commands.

## Testing

All new behavior and regressions require Rust tests.

- Domain unit tests belong beside the relevant `cama-domain` or `cama-app` module.
- Repository and migration tests use temporary SQLite databases initialized by Rust.
- Provider tests exercise typed interaction requests/responders without a live Discord gateway.
- Runtime/transport tests cover acknowledgement ordering, recovery, message delivery, and Serenity payload semantics.
- Use deterministic clocks, seeded entropy, fixtures, and fake ports. Do not add skipped or timing-flaky tests.
- Mock external Discord, OpenDota, AI, and HTTP behavior. Tests that intentionally use loopback fixture servers may require an environment that permits local binds.
- When fixing an interaction timeout, assert acknowledgement occurs before database, network, lock, or render work.

If Git history reveals a missing legacy contract, reproduce it as a Rust regression test and keep the fix in Rust.

## Common changes

### Add or change a slash command

1. Update the appropriate `*_provider.rs` command schema and handler in `cama-runtime`.
2. Put reusable business policy in `cama-app`/`cama-domain` and persistence in `cama-db`.
3. Register the provider in the production composition root when adding a new provider.
4. Add provider tests for schema, permissions, visibility, acknowledgement ordering, and success/failure behavior.
5. Verify the production registry and command-count contracts.

### Add a service or repository

1. Define the narrow typed port or repository API in the owning Rust crate.
2. Implement SQLite access in `cama-db` and orchestration in `cama-app`.
3. Wire the concrete production adapter through the runtime composition root/service container.
4. Add atomicity, retry, guild-isolation, and restart-recovery tests as applicable.

### Add a database change

1. Extend `rust/crates/cama-db/src/schema_manager.rs` and its canonical migration contract.
2. Update affected Rust repositories and domain/application types.
3. Add fresh-database and upgraded-database tests, including idempotent retry.
4. Run `cama-runtime db-check` only against a disposable database when additional audit evidence is needed.

## Collaboration

Parallelize genuinely independent work across distinct files when useful. Avoid concurrent edits to the same file, preserve user changes, recombine carefully, and run the complete relevant Rust checks after integration.
