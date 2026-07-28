# Contributing

For prerequisites, installation, and `.env` setup, see the [README](README.md).

## Making Changes

1. Create a feature branch from `main`:
   ```bash
   git checkout -b feat/your-feature
   ```

2. Make your changes and add tests for new functionality

3. Run lint checks and tests (same commands CI runs):
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
