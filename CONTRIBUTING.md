# Contributing

## Getting Started

**Prerequisites:** Python 3.12+ and [uv](https://docs.astral.sh/uv/).

1. Clone the repo and set up your environment:
   ```bash
   git clone https://github.com/cama-apps/cama-mm.git
   cd cama-mm
   uv sync --frozen
   ```

2. Create a `.env` file with your test bot token:
   ```
   DISCORD_BOT_TOKEN=your_test_token
   ADMIN_USER_IDS=your_discord_id
   ```

## Making Changes

1. Create a feature branch from `main`:
   ```bash
   git checkout -b feat/your-feature
   ```

2. Make your changes and add tests for new functionality

3. Run lint checks and tests:
   ```bash
   uv run --locked ruff check .
   uv run --locked pytest
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

All new functionality must include tests:

- **Unit tests** for domain logic (shuffler, rating, lobby)
- **Integration tests** for services and repositories
- **E2E tests** for complete workflows (see `tests/test_e2e_*.py`)

Repository tests use `repo_db_path`, which provides an initialized schema. Use `temp_db_path` only for tests that deliberately require a database without an initialized schema. Follow existing patterns.

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
