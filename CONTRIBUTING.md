# Contributing

For prerequisites, installation, and `.env` setup, see the [README](README.md).

## Making Changes

1. Create a feature branch from `main`:
   ```bash
   git checkout -b feat/your-feature
   ```

2. Make your changes and add tests for new functionality

3. Run the format, lint, and test gates (the same commands CI runs):
   ```bash
   cargo fmt --manifest-path rust/Cargo.toml --all -- --check
   cargo clippy --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features -- -D warnings
   cargo test --locked --manifest-path rust/Cargo.toml --workspace --all-targets --all-features
   ```

4. Commit with a descriptive message:
   ```bash
   git commit -m "feat: add new feature"
   ```

5. Push and open a PR:
   ```bash
   git push -u origin feat/your-feature
   ```

## Testing

All new functionality must include Rust tests:

- **Domain unit tests** beside the relevant `cama-domain` or `cama-app` module
- **Repository and migration tests** against temporary SQLite databases
  initialized by Rust
- **Provider tests** exercising typed interaction requests and responders
  without a live Discord gateway
- **Runtime/transport tests** for acknowledgement ordering, recovery, message
  delivery, and Serenity payload semantics

Use deterministic clocks, seeded entropy, fixtures, and fake ports; mock
external Discord, OpenDota, AI, and HTTP behavior. Do not add skipped or
timing-flaky tests. Follow existing patterns.

## Pull Request Process

1. Open a PR against `main`
2. CI will run tests automatically
3. A collaborator must approve the PR
4. Once approved, the PR can be merged
5. Merging triggers automatic deployment

## Branch Naming

- `feat/` - New features
- `fix/` - Bug fixes
- `chore/` - Maintenance tasks
- `docs/` - Documentation updates

## Commit Messages

Use conventional commits:
- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation
- `chore:` - Maintenance
- `ci:` - CI/CD changes
