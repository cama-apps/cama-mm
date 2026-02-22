"""
Tests to enforce architecture constraints and prevent regressions.

These tests verify that the layered architecture is maintained:
- Domain layer: Pure business logic, no infrastructure dependencies
- Service layer: Business orchestration, depends on domain and repositories
- Repository layer: Data access, depends on database infrastructure
- Command layer: Discord interface, depends on services (not repositories)
"""

import ast
import os
from pathlib import Path


def get_project_root() -> Path:
    """Get the project root directory."""
    # Tests are in tests/, so go up one level
    return Path(__file__).parent.parent


def get_imports_from_file(file_path: Path) -> set[str]:
    """Extract all import statements from a Python file."""
    imports = set()
    try:
        with open(file_path, "r", encoding="utf-8") as f:
            tree = ast.parse(f.read(), filename=str(file_path))

        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    imports.add(alias.name)
            elif isinstance(node, ast.ImportFrom):
                if node.module:
                    imports.add(node.module)
    except SyntaxError:
        # Skip files with syntax errors
        pass
    return imports


def get_all_python_files(directory: Path) -> list[Path]:
    """Get all Python files in a directory recursively."""
    return list(directory.rglob("*.py"))


class TestDomainLayerConstraints:
    """Tests for domain layer architecture constraints."""

    def test_domain_models_have_no_repository_imports(self):
        """Domain models should not import from repositories."""
        root = get_project_root()
        domain_models = root / "domain" / "models"

        if not domain_models.exists():
            return

        for file_path in get_all_python_files(domain_models):
            imports = get_imports_from_file(file_path)
            repository_imports = [
                imp for imp in imports
                if imp.startswith("repositories") or "repository" in imp.lower()
            ]
            assert not repository_imports, (
                f"{file_path.name} imports repositories: {repository_imports}. "
                "Domain models should not depend on infrastructure."
            )

    def test_domain_models_have_no_database_imports(self):
        """Domain models should not import database modules."""
        root = get_project_root()
        domain_models = root / "domain" / "models"

        if not domain_models.exists():
            return

        for file_path in get_all_python_files(domain_models):
            imports = get_imports_from_file(file_path)
            db_imports = [
                imp for imp in imports
                if imp in ("database", "sqlite3") or "database" in imp.lower()
            ]
            assert not db_imports, (
                f"{file_path.name} imports database modules: {db_imports}. "
                "Domain models should not depend on database infrastructure."
            )

    def test_domain_models_have_no_service_imports(self):
        """Domain models should not import from services layer."""
        root = get_project_root()
        domain_models = root / "domain" / "models"

        if not domain_models.exists():
            return

        for file_path in get_all_python_files(domain_models):
            imports = get_imports_from_file(file_path)
            service_imports = [
                imp for imp in imports
                if imp.startswith("services")
            ]
            assert not service_imports, (
                f"{file_path.name} imports services: {service_imports}. "
                "Domain models should not depend on service layer."
            )

    def test_domain_services_have_no_repository_imports(self):
        """Domain services should not import from repositories."""
        root = get_project_root()
        domain_services = root / "domain" / "services"

        if not domain_services.exists():
            return

        for file_path in get_all_python_files(domain_services):
            imports = get_imports_from_file(file_path)
            repository_imports = [
                imp for imp in imports
                if imp.startswith("repositories") or "repository" in imp.lower()
            ]
            assert not repository_imports, (
                f"{file_path.name} imports repositories: {repository_imports}. "
                "Domain services should not depend on infrastructure."
            )


class TestCommandLayerConstraints:
    """Tests for command layer architecture constraints."""

    def test_commands_do_not_import_repositories_directly(self):
        """Commands should use services, not repositories directly."""
        root = get_project_root()
        commands_dir = root / "commands"

        if not commands_dir.exists():
            return

        # These are allowed repository imports (TYPE_CHECKING only is OK)
        allowed_patterns = ["interfaces"]

        for file_path in get_all_python_files(commands_dir):
            # Parse the file to check for runtime repository imports
            # (TYPE_CHECKING-guarded imports are fine)
            try:
                with open(file_path, "r", encoding="utf-8") as f:
                    tree = ast.parse(f.read(), filename=str(file_path))
            except SyntaxError:
                continue

            runtime_repo_imports = []
            for node in ast.walk(tree):
                # Skip imports inside TYPE_CHECKING blocks
                if isinstance(node, ast.If):
                    test = node.test
                    if isinstance(test, ast.Name) and test.id == "TYPE_CHECKING":
                        continue
                    if isinstance(test, ast.Attribute) and test.attr == "TYPE_CHECKING":
                        continue

            # Collect only top-level / runtime imports
            for node in ast.iter_child_nodes(tree):
                if isinstance(node, (ast.Import, ast.ImportFrom)):
                    module = None
                    if isinstance(node, ast.Import):
                        for alias in node.names:
                            module = alias.name
                            if module.startswith("repositories") and not any(
                                p in module for p in allowed_patterns
                            ):
                                runtime_repo_imports.append(module)
                    elif isinstance(node, ast.ImportFrom) and node.module:
                        module = node.module
                        if module.startswith("repositories") and not any(
                            p in module for p in allowed_patterns
                        ):
                            runtime_repo_imports.append(module)
                elif isinstance(node, ast.If):
                    # Check if this is a TYPE_CHECKING guard - skip it
                    test = node.test
                    is_type_checking = False
                    if isinstance(test, ast.Name) and test.id == "TYPE_CHECKING":
                        is_type_checking = True
                    if isinstance(test, ast.Attribute) and test.attr == "TYPE_CHECKING":
                        is_type_checking = True
                    if is_type_checking:
                        continue
                    # Non-TYPE_CHECKING if blocks: check their body for imports
                    for child in ast.walk(node):
                        if isinstance(child, ast.ImportFrom) and child.module:
                            if child.module.startswith("repositories") and not any(
                                p in child.module for p in allowed_patterns
                            ):
                                runtime_repo_imports.append(child.module)

            assert not runtime_repo_imports, (
                f"{file_path.name} imports repositories at runtime: {runtime_repo_imports}. "
                "Commands should use services, not repositories directly. "
                "Use TYPE_CHECKING guards for type-hint-only imports."
            )


class TestRepositoryLayerConstraints:
    """Tests for repository layer architecture constraints."""

    def test_repositories_extend_base_repository(self):
        """All concrete repositories should extend BaseRepository."""
        root = get_project_root()
        repos_dir = root / "repositories"

        if not repos_dir.exists():
            return

        for file_path in get_all_python_files(repos_dir):
            if file_path.name in ("__init__.py", "base_repository.py", "interfaces.py"):
                continue

            with open(file_path, "r", encoding="utf-8") as f:
                content = f.read()

            # Check if it defines a class that extends BaseRepository
            if "class " in content:
                # Simple heuristic: if it's a repository file, it should import BaseRepository
                imports = get_imports_from_file(file_path)
                has_base_import = any(
                    "base_repository" in imp or "BaseRepository" in imp
                    for imp in imports
                )
                # Files with "Repository" in their class definitions should extend BaseRepository
                if "Repository" in content and "class " in content:
                    assert has_base_import or "BaseRepository" in content, (
                        f"{file_path.name} defines a Repository class but doesn't "
                        "seem to extend BaseRepository."
                    )

    def test_no_duplicate_normalize_guild_id_in_repositories(self):
        """Repositories should use BaseRepository.normalize_guild_id, not define their own."""
        root = get_project_root()
        repos_dir = root / "repositories"

        if not repos_dir.exists():
            return

        for file_path in get_all_python_files(repos_dir):
            if file_path.name == "base_repository.py":
                continue

            with open(file_path, "r", encoding="utf-8") as f:
                content = f.read()

            # Check for local _normalize_guild_id definitions
            assert "def _normalize_guild_id" not in content, (
                f"{file_path.name} defines its own _normalize_guild_id. "
                "Use BaseRepository.normalize_guild_id() instead."
            )

    def test_no_manual_begin_immediate_in_repositories(self):
        """Repositories should use atomic_transaction() context manager."""
        root = get_project_root()
        repos_dir = root / "repositories"

        if not repos_dir.exists():
            return

        for file_path in get_all_python_files(repos_dir):
            if file_path.name == "base_repository.py":
                continue

            with open(file_path, "r", encoding="utf-8") as f:
                content = f.read()

            # Check for manual BEGIN IMMEDIATE calls
            assert 'cursor.execute("BEGIN IMMEDIATE")' not in content, (
                f"{file_path.name} uses manual BEGIN IMMEDIATE. "
                "Use self.atomic_transaction() context manager instead."
            )


class TestServiceLayerConstraints:
    """Tests for service layer architecture constraints."""

    def test_services_do_not_import_commands(self):
        """Services should not depend on command layer."""
        root = get_project_root()
        services_dir = root / "services"

        if not services_dir.exists():
            return

        for file_path in get_all_python_files(services_dir):
            imports = get_imports_from_file(file_path)
            command_imports = [
                imp for imp in imports
                if imp.startswith("commands")
            ]
            assert not command_imports, (
                f"{file_path.name} imports commands: {command_imports}. "
                "Services should not depend on the command layer."
            )


class TestNoCircularImports:
    """Tests to verify there are no circular import issues."""

    def test_can_import_core_modules(self):
        """Verify core modules can be imported without circular import errors."""
        # Note: We don't clear sys.modules because that can cause false positives
        # when modules have complex initialization. Instead, we verify that
        # imports work in the normal pytest environment.

        # Just verify basic imports work
        from repositories.base_repository import BaseRepository
        from domain.models.player import Player
        from domain.models.team import Team
        from domain.models.lobby import Lobby

        assert BaseRepository is not None
        assert Player is not None
        assert Team is not None
        assert Lobby is not None


class TestBaseRepositoryPatterns:
    """Tests for BaseRepository shared patterns."""

    def test_normalize_guild_id_handles_none(self):
        """BaseRepository.normalize_guild_id should convert None to 0."""
        from repositories.base_repository import BaseRepository

        assert BaseRepository.normalize_guild_id(None) == 0
        assert BaseRepository.normalize_guild_id(0) == 0
        assert BaseRepository.normalize_guild_id(123) == 123

    def test_atomic_transaction_context_manager_exists(self):
        """BaseRepository should have atomic_transaction context manager."""
        from repositories.base_repository import BaseRepository

        assert hasattr(BaseRepository, "atomic_transaction")
        # Verify it's a method (context manager)
        import inspect
        assert inspect.isfunction(BaseRepository.atomic_transaction)
