# Python-to-Rust lift-and-shift

This workspace is the additive Rust implementation of Cama MM. During the
migration the Python bot remains production-authoritative, but the terminal
state is a true lift-and-shift: the Python container stops and `cama-rust`
becomes the sole Discord runtime and SQLite writer with the same observable
behavior. Rust becomes eligible for cutover only after every behavior contract
is mapped, tested, wired to production adapters, and exercised against the same
SQLite storage contract.

Current measured status: **6,750 of 6,750 Python test cases mapped (100%)**.
The parity ledger is complete, but operational cutover status is **5 of 13
required readiness gates complete**; **67 of 67 production runtime inventory
items are wired**, and the default and only live runtime remains Python.

## Non-negotiable invariants

1. Exactly one process owns the production Discord gateway and database writes.
   A Rust shadow never receives the production bot token. Both runtime artifacts
   take the same non-blocking advisory lock next to the SQLite file, so an
   accidental overlap fails before migration, database access, or gateway login.
2. Rust now owns clean-database initialization and all 229 existing-database
   migrations through the shared ledger. Python is retained only as differential
   and rollback evidence; the stopped Python container is not a migration or
   startup dependency.
3. Mutating differential tests use independent online-backup snapshots. They
   never point at the original dev or production database.
4. Rust preserves the existing SQLite behavior: WAL, a five-second busy timeout,
   `BEGIN IMMEDIATE` for atomic writes, foreign keys disabled, named
   `schema_migrations`, signed 64-bit IDs, and SQLite's existing type coercion.
5. Retired historical migration rows are valid. Every migration still declared
   by Python must be present, while additional ledger history is tolerated.
6. The existing Python test and deployment paths stay green throughout the work.

## Workspace

- `cama-domain`: pure policies including rating/OpenSkill updates, calibrated
  team balancing and pool shuffling, Dig economy/cave-in/gear rules, pets, mana,
  formatting, permissions, configuration, and shared interaction utilities.
- `cama-db`: Rust-owned clean/existing SQLite initialization and migration,
  compatibility checks against the retained Python schema contract, and shared
  repositories for core players/matches, `guild_config`, rating history,
  low-priority state, soft avoids, package deals, tips, player pairings, pets,
  pet evolution, pet brawls, Dig active duels/artifacts, duels, loans,
  bankruptcy, Mafia, and moderation state,
  match recording,
  mana assignment, Readycheck lobby
  state, Match Discovery administration, Dig inventory, daily economy events,
  prediction markets and settlement/rollback, Betting Service and Gambling Stats state, Manashop state, Trivia sessions,
  first-game/Dota bet seed reservations,
  Dota streak progression and ordered bonus credits, OpenDota account links,
  independently audited Dig migration postconditions, persistent Dig routes, HeroGrid aggregates, semantic Wheel spin history,
  and Wrapped year-query reads. Individual repositories deliberately remain
  free of DDL; the centralized `schema_manager` owns ordered startup migration.
- `cama-app`: application orchestration behind typed clock, randomness,
  persistence, scheduler, and Discord-transport ports. Pet, pet-brawl, duel,
  AI/SQL/flavor-service, JOPA-T post-match/routing, and `/ask`, captain-draft,
  prediction-market, pet rendering, Mafia,
  trivia data/questions/commands, dedicated-lobby, Lobby Service, Readycheck, Match Discovery, match
  recording, betting, and restart-durable voting, bankruptcy, Scout, loan,
  Mana Service, Dota bet-seed settlement policy, unified leaderboards,
  OpenDota player-profile aggregation and Dota streak scheduling,
  Registration and referrals, Dig-service/boss/flavor/asset/view/carry-wager/bonus-event/threat/prestige-4/relic/tunnel-encounter/sweep,
  loot/tunnel/route/new-event/Neon/relic-recycling hooks, HeroGrid and chart rendering, Wheel,
  balance-history aggregation, disbursement, economy-event, reminder,
  tax/lobby commands, shop, Neon Degen,
  supervised bot-task,
  and Wrapped story slices run without a live gateway while preserving
  interaction and recovery behavior. Shared lobby/match embed builders and
  interaction-safety/channel policies preserve Discord limits, lane results,
  identity alignment, bulk penalty lookups, attachment rewind, and
  public-to-private fallback ordering.
- `cama-runtime`: the production bot entrypoint under construction. The checked-in
  implementation starts a supervised Tokio/Serenity gateway and provides typed
  command, component, response, worker, and lifecycle boundaries. Its immutable
  startup configuration graph covers every one of `config.py`'s 215 environment
  keys, provider-bound/redacted secrets, derived aliases, and migration inputs;
  a Python-AST drift test keeps that catalog exact. The
  mechanical runtime inventory reports all 67 production items wired. Global
  command replacement remains explicitly cutover-gated by the operational
  readiness manifest, so a complete inventory or gateway connection alone cannot
  be mistaken for production readiness.
- `xtask`: machine-checked Python/Rust test inventory, migration-manifest drift,
  and cutover-completeness gates.
- `parity/tests.tsv`: exact Python pytest case to Rust test mappings.
- `parity/cutover_readiness.tsv`: required operational gates that keep cutover
  blocked even after numeric test parity until live adapters and rehearsals have
  explicit evidence.
- `parity/domain_vectors.tsv`: 230 language-neutral inputs replayed through the
  production Python functions and Rust policies in one differential gate,
  including OpenSkill probabilities, exact persona/cave-in catalog fingerprints,
  deterministic pet evolution and brawls, LLM provider/credential selection,
  Dig reward, wager, miner-stat, and all authored boss-phase-event boundaries,
  wheel and Wrapped flavor catalogs, AI SQL-validation guardrails, `/ask` result formatting,
  disbursement embeds, and Discord
  embed truncation/packing/validation.
- `parity/python_vectors.py`: the long-lived Python side of that vector runner.
- `parity/baseline.txt`: fingerprint of the entire current pytest collection.
- `schema/expected_migrations.txt`: ordered migration contract exported from
  Python and verified on every CI run.

Further bounded-context crates or modules cover integrations, rendering, and
runtime workers as those slices are ported. Typed ports and policy tests are not
cutover-complete until concrete production adapters are composed by
`cama-runtime` and exercised with Python stopped.

## Local gates

Run from the repository root:

```bash
uv run --locked ruff check .
uv run --locked pytest
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features -- -D warnings
cargo test --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features
cargo run --locked --manifest-path rust/Cargo.toml -p xtask -- parity
uv run --locked python rust/scripts/generate_authored_asset_manifest.py --check
uv run --locked python scripts/visual_equivalence.py
```

For an offline/local checkout that already has the locked Python environment,
the verifier can use that interpreter without changing CI's default `uv`
contract:

```bash
CAMA_PARITY_PYTHON=.venv/bin/python cargo run --locked --manifest-path rust/Cargo.toml -p xtask -- parity
```

The root `.python-version` pins local `uv` commands to CPython 3.12, matching
the CI runtime.

The optional visual-equivalence development check exercises the production
Python and Rust prediction-chart,
balance-journey, rating-history, calibration rating-distribution,
rating-analysis, advantage, profile, pet, wheel/explosion, Blame Luke, scout,
Hero Grid, post-match GIF, terminal-crash GIF, and pinnacle phase-three
renderers from one deterministic fixture. It remains useful regression evidence
for matching dimensions, animation frame order/timing/loop count, bounded RGBA
error, foreground spatial overlap, and media-specific semantic colors, but it
is not a cutover requirement. The `image_and_attachment_equivalence` readiness
gate instead requires an isolated staging proof against real Discord of correct
asset selection, filename/type/dimensions, behavior-relevant animation timing,
and attachment replace/preserve/clear/retry behavior. Pixel/perceptual matching
and font equivalence are not required for that gate.

`xtask parity` fails if Python's migration list changes, if pytest node IDs
change without review, if a mapped Rust test disappears or is ambiguous across
crates, or if a Python/Rust domain vector differs. It also exercises thirteen
repository families serially on one disposable SQLite file and reports the
largest remaining Python modules. During development it reports incremental
progress. The cutover gate is stricter:

```bash
cargo run --locked --manifest-path rust/Cargo.toml -p xtask -- parity --require-complete
```

A complete **6,750/6,750** parity ledger is necessary but not sufficient: this
command must remain failing until all 6,750 current cases—and any cases added
later—have an explicit passing Rust contract, and every required gate in
`parity/cutover_readiness.tsv` is marked complete with evidence. The required
gate names are compiled into `xtask`, so deleting an open row cannot make the
cutover check pass.

## Safe development-database validation

The ignored root `cama_shuffle.db` is a real development snapshot. Normal
pytest tests do not use it. Never start either runtime or run a mutating test
against that original file merely to check compatibility.

Create an online backup, let Python upgrade only the copy, then audit it with
Rust:

```bash
CAMA_RUST_SNAPSHOT_DIR="$(mktemp -d /tmp/cama-rust-snapshot.XXXXXX)"
sqlite3 cama_shuffle.db ".backup '$CAMA_RUST_SNAPSHOT_DIR/cama_shuffle.db'"
uv run --locked python -c 'import sys; from database import Database; db = Database(sys.argv[1]); db.close()' "$CAMA_RUST_SNAPSHOT_DIR/cama_shuffle.db"
sqlite3 "$CAMA_RUST_SNAPSHOT_DIR/cama_shuffle.db" 'PRAGMA wal_checkpoint(TRUNCATE);'
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- db-check --db-path "$CAMA_RUST_SNAPSHOT_DIR/cama_shuffle.db"
```

On 2026-08-11, the original local snapshot was healthy but 26 current migrations
behind. The copied upgrade had all 229 current migrations plus two legitimate
retired ledger rows and passed the Rust preflight. The original remained at 203
rows. This is exactly why the clone step is mandatory.

A writable repository slice can then be exercised only on that
disposable, fully migrated copy:

```bash
cargo run --locked --manifest-path rust/Cargo.toml -p cama-db \
  --example guild_config_snapshot_smoke -- \
  "$CAMA_RUST_SNAPSHOT_DIR/cama_shuffle.db" --disposable-copy
```

The referral settlement probe has a direct no-Python test path as well. It
creates a fresh Rust-migrated SQLite database, checks the exact
`--disposable-copy` write guard before opening it, then runs the live
`CoreMatchRecord` settlement and verifies table deltas, idempotent retry,
cross-guild isolation, semantic `jc_changes`, and scoped ledger metadata:

```bash
cargo test --locked --manifest-path rust/Cargo.toml -p cama-db \
  --example referral_snapshot_smoke
```

The older mixed-runtime development rehearsal is available as a machine-readable,
fail-closed runner. It keeps the clone in a temporary directory, records
source immutability before and after, runs Python migration on the clone, Rust
`db-check`, the fourteen-family repository smoke, an additional live
match-core/referral settlement smoke, and Survey recovery smoke, then prints a
normalized JSON report (use a new `--report` path to retain it). The referral
probe is an extra production callsite check, not a fifteenth shared-repository
family.
The clone is Python-era SQLite storage: Python only migrates that copy, and
Rust is the only post-migration reader/writer. It does not perform
Rust-to-Python readback:

```bash
CAMA_PARITY_PYTHON=.venv/bin/python \
  python scripts/production_snapshot_replay.py \
  /path/to/cama_shuffle.db --report /tmp/cama-snapshot-replay.json
```

This is development evidence only; it does not close the cutover readiness
gate or replace the broader repository and backup-rollback rehearsal
requirements.

For the actual one-way cutover rehearsal, use a separately created disposable copy
and run the Rust-only harness below. It requires both paths so it can hash the
source before and after; it refuses the same file, SQLite sidecars, missing
copies, and reports that would overwrite either database. The harness does not
run Python migration and never asks Python to inspect the Rust-mutated copy.
The production `cama-rust db-admit` command invokes the same schema migration
and compatibility audit as startup, then Rust repository, Profile/Info,
Dig/Pet, referral, and Survey recovery smokes exercise only the disposable copy:

```bash
sqlite3 /path/to/cama_shuffle.db \
  ".backup '/tmp/cama-rust-cutover-copy.db'"
python scripts/rust_cutover_rehearsal.py \
  /path/to/cama_shuffle.db \
  --disposable-copy /tmp/cama-rust-cutover-copy.db \
  --report /tmp/cama-rust-cutover-rehearsal.json
```

The JSON evidence is marked `mode=rust_only_one_way` and records Rust
admission/migration counts, every self-checking Rust smoke command, the final
Rust database health check, and `source.unchanged=true`. A passing report is a disposable
copy/cutover rehearsal result; it does not itself authorize deployment or
replace the tested backup-and-rollback path.

The narrower A/B evidence runner uses two independent copies: it completes the
retained-Python repository writes only on the Python copy, and runs the Rust
writes only on the second copy. It compares stable, schema-aware projections
for guild configuration, Dig inventory/route state, Survey delivery, and
first-match referral settlement (including normalized row deltas and source
immutability), rather than SQLite file bytes:

```bash
CAMA_PARITY_PYTHON=.venv/bin/python \
  python scripts/production_snapshot_ab_delta.py \
  /path/to/cama_shuffle.db --report /tmp/cama-snapshot-ab-delta.json
```

This bounded current-schema representative remains optional development evidence.
It is not a cutover round-trip requirement: after Rust takes ownership, Python
never opens the Rust-mutated database. The
`backup_rollback_rehearsal` gate carries the tested rollback proof through the
verified database backup and retained Python-image restoration path.

The parity command's separate Python-to-Rust migration bridge covers thirteen
repository families. The shared Rust snapshot smoke below covers fourteen, so
the family counts intentionally differ between those two checks. The
standalone replay and A/B runner additionally exercise the live
`CoreMatchRecord` referral settlement path; that focused probe does not change
the fourteen-family smoke count.

The explicit confirmation is intentional: this smoke writes reserved negative
sentinels through fourteen shared repository families: guild configuration,
economy-event policy, low-priority state, soft avoids, package deals, tips,
player pairings, loans, Dig inventory and routes, pets, semantic Wheel spin
history, pet brawls, and duels. It must never be pointed at the original
workspace or server database.

## CI and deployment

The existing `CI` workflow still requires the full Python lint and test suite.
Its Rust job additionally requires formatting, Clippy, unit tests, the parity
gate, Python→Rust SQLite migration interoperability, builds of both production
images, a Python import smoke, and a Rust preflight against a freshly
Python-migrated database. Setting the repository variable
`RUST_CUTOVER_CANDIDATE=true` enables `--require-complete` as a hard CI gate.
The Rust job also runs `scripts/test-operational-rehearsal` with the built Rust
image. It starts `cama-rust health-smoke` against a disposable Python-migrated
database with Docker networking disabled. The real runtime supervisor and
health reporter dispatch one registered command through a recording Discord
responder, commit and read its `app_kv` write, and prove that the health probe
rejects the stopped marker. The same rehearsal uses a temporary WAL-mode SQLite
source to exercise the real online-backup helper, verify backup metadata and
immutability, reject destination overwrite, and check the deploy workflow's
exact SHA/runtime handoff. It then runs the real deploy script in a temporary
root against the recording Docker/Compose shim to force an unhealthy Rust
candidate and prove automatic restoration of the retained Python image. The
local form omits `--rust-health-image` and therefore needs no Docker; neither
form touches the development database or needs SSH, external network access,
or Discord credentials. The recording responder exercises the typed interaction
boundary, not Serenity's HTTP transport or a live gateway. The
`container_health_smoke` gate therefore remains open only for an isolated
staging-guild Discord write and health proof with Python stopped.

For a Rust service already running through the composed `bot` service, the
black-box post-deploy verifier checks the selected runtime and revision, UID
1001, the `/app/data` bind mount, Docker health, and (when present) the OCI
revision label:

```bash
BOT_RUNTIME=rust GIT_SHA="$DEPLOY_SHA" ./scripts/post-deploy-verify
```

It also runs `cama-rust health-check` inside the container. Recent Rust logs
must be JSON and must contain the structured `starting Rust Discord runtime`
record with matching `runtime` and `git_sha`; missing startup metadata or a
non-JSON record fails closed. That check depends on the Rust startup tracing
hook and is evidence for the running container only—it does not establish a
live Discord gateway, alert delivery, or close a cutover-readiness gate.

The default Compose graph still starts only the Python `bot`. All deployments
now go through a checked same-service selector; leaving `BOT_RUNTIME` unset is
equivalent to selecting `python`:

```bash
GIT_SHA="$DEPLOY_SHA" ./scripts/deploy-runtime
BOT_RUNTIME=python GIT_SHA="$DEPLOY_SHA" ./scripts/deploy-runtime
```

Those Python selections are valid only before the Rust cutover marker exists.
After any Rust cutover begins, `deploy-runtime`, `runtime-compose`, and the
Python image entrypoint reject Python explicitly. The only Python fallback is
the automatic failed-cutover path, which stops Rust, restores the verified
pre-cutover database, durably removes the pending marker, and only then starts
the retained Python image.

The eventual cutover uses that exact path with both hard gates explicit:

```bash
RUST_CUTOVER_CANDIDATE=true BOT_RUNTIME=rust GIT_SHA="$DEPLOY_SHA" \
  ./scripts/deploy-runtime
```

It keeps the
`bot` service name, UID 1001, `.env`, data bind mount, database path, restart
policy, and `GIT_SHA`, while replacing only the Dockerfile and command with
`Dockerfile.rust` and `serve`. The helper currently refuses this selection
unless the candidate flag enabled CI's complete parity job and the readiness
manifest has no open gates; it also rejects
empty, unknown, or case-mismatched values before invoking Compose. Both images
independently reject a selector/image mismatch.

`deploy-runtime` builds both artifacts while the old process stays live, then
stops it and verifies it is stopped before making the authoritative SQLite
backup. Rust admits a disposable copy of that backup before it can touch live
SQLite. The candidate must pass runtime health and the full post-deploy verifier.
Any failure stops Rust, atomically restores the verified pre-cutover database,
and only then starts the retained Python image. A durable cutover marker blocks
all later Python selections at both the deployment and process boundaries;
subsequent deployments and rollbacks are Rust to Rust. Python is never pointed
at a database Rust has mutated.

For configuration inspection and ordinary Compose operations without the
backup/deploy transaction, use the same selector wrapper:

```bash
./scripts/runtime-compose config
BOT_RUNTIME=rust ./scripts/runtime-compose config
```

An opt-in offline schema preflight is built and run through the same Compose
project and data path:

```bash
docker compose --profile rust-preflight run --rm rust-preflight
```

It has no usable Discord token and the Rust binary opens SQLite read-only. The
Rust service override exists for mechanical validation, but the guarded deploy
path refuses to start it until gateway, worker, repository, and operational
parity exist. The bind directory remains writable because WAL readers must
coordinate through SQLite sidecars; the main preflight database connection is
still opened with `SQLITE_OPEN_READ_ONLY` and `query_only`.

## Cutover safety bar

The historical Python/Rust mapping remains useful regression evidence, but a
Python readback or A/B round trip is not part of the one-way production gate.
Cutover requires these operational properties:

- the production Rust binary admits a copied pre-cutover SQLite snapshot and
  passes its Rust-only repository/recovery probes;
- command schema, interaction timing, ephemeral/mention/component, persistent
  view, reconnect, and scheduled-worker parity;
- recorded offline fixtures for OpenDota, Dotabase, Steam assets, and LLM
  provider requests/fallbacks;
- functional media checks for upload/edit/retry and attachment
  replace/preserve/clear behavior under real Discord; pixel identity is not required;
- a copied-dev-database migration rehearsal, integrity checks, restart recovery,
  container smoke test, backup, rollback, and verified single-writer cutover.

At cutover, Python is quiesced, a backup is verified, and the single
Compose `bot` service changes runtime while retaining UID 1001, `.env`, assets,
`GIT_SHA`, `./data:/app/data`, and `/app/data/cama_shuffle.db`. Python rollback
is permitted only after restoring that verified pre-cutover backup; once Rust
is admitted successfully, the deployment selector is permanently Rust-only.

## Post-parity Python/Rust benchmarks

Performance measurement starts only after the parity ledger, live-runtime
inventory, and correctness gates are complete. The benchmark suite will run the
same deterministic input fixtures through both implementations and reject a
trial if their normalized outputs differ. At minimum it covers calibrated lobby
shuffle/search, paired test-suite execution, sustained runtime/event-loop work,
rating and analytics calculations, SQLite-heavy application operations on
independent copies of one snapshot, and representative native image/GIF
rendering. Test execution is measured after the Rust test binaries are built;
cold compilation and Python import/collection costs are reported separately so
they cannot distort the execution comparison.

Rust is measured from an optimized release build; both runtimes receive warm-up
runs, use the same machine and concurrency, and run enough randomized-order
iterations to report distributions rather than a best-case sample. The recorded
artifact includes wall and CPU time, operations per second, p50/p95/p99 latency,
and peak resident memory, along with fixture hashes, runtime/compiler versions,
commit SHA, and raw results. Startup and idle/sustained-running memory are
reported separately from CPU-bound throughput so neither runtime gets an
artificially favorable aggregate number.
