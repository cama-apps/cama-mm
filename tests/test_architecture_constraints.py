"""
Tests to enforce architecture constraints and prevent regressions.

These tests verify that the layered architecture is maintained:
- Domain layer: Pure business logic, no infrastructure dependencies
- Service layer: Business orchestration, depends on domain and repositories
- Repository layer: Data access, depends on database infrastructure
- Command layer: Discord interface, depends on services (not repositories)
"""

import ast
from pathlib import Path


def get_project_root() -> Path:
    """Get the project root directory."""
    # Tests are in tests/, so go up one level
    return Path(__file__).parent.parent


def get_imports_from_file(file_path: Path) -> set[str]:
    """Extract all import statements from a Python file."""
    imports = set()
    try:
        with open(file_path, encoding="utf-8") as f:
            tree = ast.parse(f.read(), filename=str(file_path))

        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    imports.add(alias.name)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imports.add(node.module)
    except SyntaxError:
        # Skip files with syntax errors
        pass
    return imports


def get_all_python_files(directory: Path) -> list[Path]:
    """Get all Python files in a directory recursively."""
    return list(directory.rglob("*.py"))


def _attr_chain(node: ast.AST) -> list[str] | None:
    """Return dotted-name parts for a call target, if it is attribute/name based."""
    parts: list[str] = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
    elif isinstance(node, ast.Call):
        parts.append("<call>")
    else:
        return None
    return list(reversed(parts))


def _is_asyncio_to_thread(call: ast.Call) -> bool:
    return _attr_chain(call.func) in (["asyncio", "to_thread"], ["to_thread"])


class _AsyncCallScanner(ast.NodeVisitor):
    """Scan async functions while ignoring nested sync helpers/lambdas."""

    def __init__(
        self,
        file_path: Path,
        *,
        reason_for_call,
    ):
        self.file_path = file_path
        self.reason_for_call = reason_for_call
        self.async_stack: list[str] = []
        self.parents: list[ast.AST] = []
        self.findings: list[str] = []

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef):
        self.async_stack.append(node.name)
        for stmt in node.body:
            self.visit(stmt)
        self.async_stack.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef):
        return

    def visit_Lambda(self, node: ast.Lambda):
        return

    def visit_Call(self, node: ast.Call):
        chain = _attr_chain(node.func)
        reason = self.reason_for_call(chain)
        inside_to_thread = any(
            isinstance(parent, ast.Call) and _is_asyncio_to_thread(parent)
            for parent in self.parents
        )
        if reason and not _is_asyncio_to_thread(node) and not inside_to_thread:
            call_name = ".".join(chain or [])
            async_name = self.async_stack[-1] if self.async_stack else "<module>"
            self.findings.append(f"{self.file_path}:{node.lineno}:{async_name}:{call_name} ({reason})")

        self.parents.append(node)
        self.generic_visit(node)
        self.parents.pop()


class TestDomainLayerConstraints:
    """Tests for domain layer architecture constraints."""

    def test_domain_models_have_no_repository_imports(self):
        """Domain models should not import from repositories."""
        root = get_project_root()
        domain_models = root / "domain" / "models"

        assert domain_models.is_dir(), f"expected {domain_models} to exist"

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

        assert domain_models.is_dir(), f"expected {domain_models} to exist"

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

        assert domain_models.is_dir(), f"expected {domain_models} to exist"

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

        assert domain_services.is_dir(), f"expected {domain_services} to exist"

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

        assert commands_dir.is_dir(), f"expected {commands_dir} to exist"

        # These are allowed repository imports (TYPE_CHECKING only is OK)
        allowed_patterns = ["interfaces"]

        for file_path in get_all_python_files(commands_dir):
            # Parse the file to check for runtime repository imports
            # (TYPE_CHECKING-guarded imports are fine)
            try:
                with open(file_path, encoding="utf-8") as f:
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
                        if (
                            isinstance(child, ast.ImportFrom)
                            and child.module
                            and child.module.startswith("repositories")
                            and not any(p in child.module for p in allowed_patterns)
                        ):
                            runtime_repo_imports.append(child.module)

            assert not runtime_repo_imports, (
                f"{file_path.name} imports repositories at runtime: {runtime_repo_imports}. "
                "Commands should use services, not repositories directly. "
                "Use TYPE_CHECKING guards for type-hint-only imports."
            )

    def test_async_command_paths_offload_blocking_work(self):
        """Async Discord paths should not run sync service/repo or drawing work inline."""
        root = get_project_root()
        paths = get_all_python_files(root / "commands") + [root / "bot.py"]

        service_names = {
            "ai_service",
            "balance_history_service",
            "bankruptcy_service",
            "betting_service",
            "dig_service",
            "disburse_service",
            "draft_service",
            "enrichment_service",
            "flavor_text_service",
            "gambling_stats_service",
            "guild_config_service",
            "lobby_service",
            "loan_service",
            "mana_effects_service",
            "mana_service",
            "match_service",
            "opendota_player_service",
            "package_deal_service",
            "player_service",
            "prediction_service",
            "recalibration_service",
            "rebellion_service",
            "reminder_service",
            "soft_avoid_service",
            "sql_query_service",
            "tip_service",
            "wrapped_service",
        }
        repo_like_names = {
            "bankruptcy_repo",
            "bet_repo",
            "draft_state_manager",
            "lobby_manager",
            "mana_repo",
            "match_repo",
            "pairings_repo",
            "player_repo",
            "prediction_repo",
        }
        known_blocking_helpers = {
            "compose_items_used",
            "compose_shop_grid",
            "draw_balance_chart",
            "draw_calibration_curve",
            "draw_gamba_chart",
            "draw_hero_grid",
            "draw_hero_performance_chart",
            "draw_lane_distribution",
            "draw_market_fair_history",
            "draw_matches_table",
            "draw_prediction_over_time",
            "draw_rating_comparison_chart",
            "draw_rating_distribution",
            "draw_rating_history_chart",
            "draw_role_graph",
            "draw_scout_report",
            "ensure_cached",
            "get_boss_art",
            "get_event_art",
            "get_item_art",
            "get_pickaxe_art",
            "get_trivia_image",
        }
        known_async_methods = {
            "flavor",
            "generate_data_insight",
            "generate_event_flavor",
            "narrate_boss_fight",
            "narrate_splash",
            "notify_betting_subscribers",
            "on_100_bets_milestone",
            "on_all_in_bet",
            "on_balance_check",
            "on_bankruptcy",
            "on_bet_placed",
            "on_bet_settled",
            "on_bomb_pot",
            "on_captain_symmetry",
            "on_cooldown_hit",
            "on_degen_milestone",
            "on_double_or_nothing",
            "on_draft_coinflip",
            "on_first_leverage_bet",
            "on_gamba_spectator",
            "on_games_milestone",
            "on_last_second_bet",
            "on_leverage_loss",
            "on_lightning_bolt",
            "on_lobby_join",
            "on_loan",
            "on_match_enriched",
            "on_match_recorded",
            "on_prediction_resolved",
            "on_registration",
            "on_rivalry_detected",
            "on_simultaneous_events",
            "on_soft_avoid",
            "on_tip",
            "on_unanimous_wrong",
            "on_wheel_result",
            "on_win_streak_record",
            "query",
            "reschedule_all",
        }
        allowed_pure_leaf_calls = {
            "_get_balance_history_service",
            "_get_bankruptcy_service",
            "_get_gambling_stats_service",
            "_get_loan_service",
            "_get_match_repo",
            "_get_pairings_repo",
            "_get_player_repo",
            "_get_prediction_service",
            "_get_tip_service",
            "calculate_threshold",
            "compute_repair_cost",
            "get_creation_lock",
            "get_layer",
            "roll_battle",
        }
        allowed_pure_calls = {
            "self.dig_service._force_event_for.add",
            "self.disburse_service.METHOD_LABELS.get",
        }

        def reason_for_call(chain: list[str] | None) -> str | None:
            if not chain:
                return None
            leaf = chain[-1]
            full = ".".join(chain)
            if leaf in known_async_methods or leaf in allowed_pure_leaf_calls or full in allowed_pure_calls:
                return None
            if leaf in known_blocking_helpers:
                return "known blocking helper"
            if any(
                part in service_names
                or part in repo_like_names
                or part.endswith("_repo")
                or part.endswith("_repository")
                for part in chain
            ):
                return "sync service/repo call"
            return None

        findings = []
        for path in paths:
            try:
                tree = ast.parse(path.read_text(), filename=str(path))
            except SyntaxError:
                continue
            scanner = _AsyncCallScanner(path.relative_to(root), reason_for_call=reason_for_call)
            scanner.visit(tree)
            findings.extend(scanner.findings)

        assert not findings, "Blocking calls in async command paths:\n" + "\n".join(findings)


class TestServiceAsyncConstraints:
    """Tests for async service methods used by command paths."""

    def test_async_service_methods_offload_repository_calls(self):
        """Async services should not do synchronous repository work on the event loop."""
        root = get_project_root()
        services_dir = root / "services"

        repo_like_names = {
            "ai_query_repo",
            "bankruptcy_repo",
            "bet_repo",
            "dig_repo",
            "guild_config_repo",
            "lobby_repo",
            "loan_repo",
            "match_repo",
            "notification_repo",
            "player_repo",
            "prediction_repo",
        }
        service_like_names = {
            "bankruptcy_service",
            "dig_service",
            "gambling_stats_service",
            "loan_service",
            "match_service",
            "player_service",
        }
        known_async_methods = {
            "complete",
            "flavor",
            "generate_flavor",
            "generate_sql",
            "narrate_boss_fight",
            "narrate_splash",
            "notify_betting_subscribers",
            "reschedule_all",
        }

        def reason_for_call(chain: list[str] | None) -> str | None:
            if not chain or chain[-1] in known_async_methods:
                return None
            if any(
                part in repo_like_names
                or part in service_like_names
                or part.endswith("_repo")
                or part.endswith("_repository")
                for part in chain
            ):
                return "sync service/repo call"
            return None

        findings = []
        for path in get_all_python_files(services_dir):
            try:
                tree = ast.parse(path.read_text(), filename=str(path))
            except SyntaxError:
                continue
            scanner = _AsyncCallScanner(path.relative_to(root), reason_for_call=reason_for_call)
            scanner.visit(tree)
            findings.extend(scanner.findings)

        assert not findings, "Blocking calls in async service methods:\n" + "\n".join(findings)


class TestRepositoryLayerConstraints:
    """Tests for repository layer architecture constraints."""

    def test_repositories_extend_base_repository(self):
        """All concrete repositories should extend BaseRepository."""
        root = get_project_root()
        repos_dir = root / "repositories"

        assert repos_dir.is_dir(), f"expected {repos_dir} to exist"

        for file_path in get_all_python_files(repos_dir):
            if file_path.name in ("__init__.py", "base_repository.py", "interfaces.py"):
                continue

            with open(file_path, encoding="utf-8") as f:
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

        assert repos_dir.is_dir(), f"expected {repos_dir} to exist"

        for file_path in get_all_python_files(repos_dir):
            if file_path.name == "base_repository.py":
                continue

            with open(file_path, encoding="utf-8") as f:
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

        assert repos_dir.is_dir(), f"expected {repos_dir} to exist"

        for file_path in get_all_python_files(repos_dir):
            if file_path.name == "base_repository.py":
                continue

            with open(file_path, encoding="utf-8") as f:
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

        assert services_dir.is_dir(), f"expected {services_dir} to exist"

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
