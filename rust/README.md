# Rust runtime

`cama-rust` is the sole production runtime and the repository's only
implementation. Historical behavior can be recovered from Git history when
needed.

## Draft analysis

Successful match enrichment also requests a hero-composition estimate from Batru's
free API. OpenDota's `radiant_captain` and `dire_captain` account IDs identify the
hero drafters and are mapped to the participants' linked Discord accounts. No
STRATZ or Batru API key is needed. Unknown captains remain unknown.

Post-game and match-history embeds display both percentages, the drafters, and a
Batru attribution link. Inclusive 48–52% estimates count as split drafts; missing
estimates do not. Draft results are informational and do not affect match wins,
ratings, rewards, or team balancing.

- `/enrich drafts` runs a background backfill of all linked historical matches
  (admin only). `/enrich drafts status:true` shows progress. It reuses saved
  OpenDota responses, fetches missing data when needed, and updates only draft
  analysis. Calls are paced and share Batru's 60-request/minute allowance with
  normal enrichment. Discord progress-message expiry does not stop the job.
  Rerun after a process restart or provider outages; existing predictions retain
  their original timestamp.
- `/matches drafts [view:matches] [sort:recent|imbalance] [page:1] [user] [limit:5]` browses all
  recorded matches, or ranks estimated drafts by imbalance. Previous/Next buttons
  preserve the sort and drafter filter. Each entry includes the actual game winner,
  draft percentages, captains, and external match links when a Valve ID is available.
- `/matches drafts view:best_drafters` or `view:worst_drafters` ranks captains by
  draft win rate, showing wins, losses, splits, and sample sizes. Drafters with
  no decisive draft estimates are excluded from the rankings.
- Player profiles show draft win rate: favored drafts divided by favored plus
  unfavored drafts. Splits and unavailable estimates are listed separately.

Historical estimates use Batru's model at the time of calculation, not a
reconstruction of the model for the historical Dota patch. The database stores
the probability, classification, hero lineup, provider, and calculation time.
Provider failures leave an unavailable estimate without blocking match recording.

## Workspace

- `cama-domain`: storage- and transport-independent policy and models.
- `cama-db`: SQLite schema ownership, migrations, audits, and repositories.
- `cama-app`: application services and typed external ports.
- `cama-runtime`: Tokio/Serenity providers, workers, health, and composition.

Repository-wide architecture contracts are compiled into the
`cama-runtime` test target. The retired cross-language parity `xtask` and its
database conversion probes have been removed.

## Local checks

Run the required gates from the repository root:

```bash
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path rust/Cargo.toml \
  --workspace --all-targets --all-features -- -D warnings
cargo test --locked --manifest-path rust/Cargo.toml \
  --workspace --all-targets --all-features
```

The full suite includes loopback HTTP fixture servers. Run it in an environment
that permits binding localhost ports.

## Database ownership

Rust owns the current schema and migration ledger. Production startup opens the
configured existing SQLite database and applies any future Rust migrations.
Repositories never create or migrate databases themselves.

Tests that need the current schema restore isolated in-memory databases from a
single Rust-created template. Tests that exercise migration behavior create
their own temporary legacy shapes explicitly; they do not invoke a retired
runtime or rebuild an externally migrated database for each case.

Never mutate `cama_shuffle.db` merely to validate it. Use a disposable copy for
manual checks:

```bash
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- \
  db-check --db-path /path/to/disposable/cama_shuffle.db
```

`db-admit` additionally requires distinct source and candidate copies and is
used by deployment before a candidate container starts.

## Runtime data and container image

Release resolves the latest published Dotabase version from package metadata,
downloads its source archive, verifies the publisher SHA-256, and extracts only
`dotabase.db`. Renderer fonts are also downloaded as verified data files. The
staging step uses shell tooling only; it does not install a Python package or
interpreter:

```bash
./scripts/stage-runtime-assets
```

`Dockerfile.rust` then packages the prebuilt release binary and staged data into
a Debian runtime image. It is a deterministic packaging build; dependency and
asset discovery happen before Docker. A local builder target remains available
for development builds.

## Running locally

The Compose graph is Rust-only:

```bash
docker compose up --build bot
```

Or run the binary directly; `DB_PATH` defaults to `cama_shuffle.db`:

```bash
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- serve
```

Useful read-only checks:

```bash
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- inventory
cargo run --locked --manifest-path rust/Cargo.toml -p cama-runtime -- \
  health-check --maximum-age-seconds 120
```

## CI, release, and deployment

Pull-request CI has two parallel Rust jobs:

- the complete workspace test suite;
- formatting plus Clippy for all targets and features.

Both restore Cargo build artifacts. They run concurrently so Clippy's check
artifacts are no longer placed serially ahead of the test suite. Workspace
source changes still require the affected crates and test binaries to be
rebuilt; dependency caching cannot reuse an old linked workspace binary after
its source changes.

The main-branch Release workflow builds `cama-rust` on Ubuntu 22.04, stages the
latest verified runtime data, and publishes an immutable image tagged with the
commit SHA. Deploy consumes only that Rust image. It creates and admits a
disposable database backup before startup and restores both the previous Rust
image and exact SQLite file set if verification fails.

The revision is carried end to end:

- Release passes `github.sha` as Docker's `GIT_SHA` build argument.
- The image stores it in `GIT_SHA` and
  `org.opencontainers.image.revision`.
- Deployment verifies the immutable image label before startup.
- Post-deploy verification checks the container environment, health output,
  and structured startup log.
- The admin status command reads `GIT_SHA` and displays its short form.

Deployment is triggered only after the Release workflow succeeds on `main`.
The active workflows and runtime scripts are Rust-only and contain no
legacy-runtime selector or rollback path.
