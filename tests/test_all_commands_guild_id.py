"""Static checks for guild_id definition-before-use and command imports."""

import ast
from pathlib import Path

import pytest

COMMANDS_DIR = Path(__file__).parent.parent / "commands"



def get_python_files() -> list[Path]:
    """Get all Python files in the commands directory."""
    return list(COMMANDS_DIR.glob("*.py"))


def find_guild_id_issues_in_file(filepath: Path) -> list[tuple[int, str]]:
    """
    Analyze a file for potential guild_id issues.

    Returns list of (line_number, issue_description) tuples.
    """
    issues = []
    content = filepath.read_text(encoding='utf-8')

    # Pattern: using guild_id before it's defined in a function
    # This is a simplified check - we look for functions that use guild_id
    # and verify that guild_id is defined before its first use

    try:
        tree = ast.parse(content)
    except SyntaxError:
        issues.append((0, f"Syntax error in {filepath.name}"))
        return issues

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            issues.extend(_check_function_for_guild_id_issues(node, filepath.name))

    return issues


def _check_function_for_guild_id_issues(func: ast.FunctionDef, filename: str) -> list[tuple[int, str]]:
    """Check a single function for guild_id usage issues."""
    issues = []

    # Track where guild_id is defined vs used
    guild_id_definitions = []  # Line numbers where guild_id is assigned
    guild_id_uses = []  # Line numbers where guild_id is used in a call

    for node in ast.walk(func):
        # Check for guild_id assignment
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "guild_id":
                    guild_id_definitions.append(node.lineno)

        # Check for guild_id in function calls as a positional or keyword arg
        if isinstance(node, ast.Call):
            # Check if any argument is the name 'guild_id'
            for arg in node.args:
                if isinstance(arg, ast.Name) and arg.id == "guild_id":
                    guild_id_uses.append(node.lineno)

            for keyword in node.keywords:
                if (
                    keyword.arg == "guild_id"
                    and isinstance(keyword.value, ast.Name)
                    and keyword.value.id == "guild_id"
                ):
                    guild_id_uses.append(node.lineno)

    # Check if guild_id is used before defined
    if guild_id_uses and guild_id_definitions:
        first_use = min(guild_id_uses)
        first_def = min(guild_id_definitions)
        if first_use < first_def:
            issues.append((
                first_use,
                f"{filename}:{func.name}: guild_id used on line {first_use} "
                f"before defined on line {first_def}"
            ))

    return issues


class TestGuildIdDefinedBeforeUse:
    """Verify guild_id is defined before use in command call arguments."""

    @pytest.mark.parametrize("filepath", get_python_files(), ids=lambda p: p.name)
    def test_guild_id_defined_before_use(self, filepath: Path):
        """
        Verify that guild_id is defined before it's used in function calls.

        This catches the pattern:
            player = player_service.get_player(user_id, guild_id)  # ERROR: guild_id not defined
            guild_id = interaction.guild.id if interaction.guild else None  # Too late!
        """
        issues = find_guild_id_issues_in_file(filepath)

        if issues:
            issue_msgs = [f"  Line {line}: {msg}" for line, msg in issues]
            pytest.fail(
                f"guild_id usage issues in {filepath.name}:\n" + "\n".join(issue_msgs)
            )




class TestCommandFilesExist:
    """Verify the maintained command modules import successfully."""

    def test_all_command_files_can_be_imported(self):
        """Verify all command modules can be imported without errors.

        Uses importlib.import_module instead of `import commands.X` +
        attribute access. Under xdist parallel runs, sys.modules can already
        contain a submodule (cached by another worker's collection) without
        the parent `commands` package having the attribute bound — this made
        the previous form flake intermittently with "module 'commands' has
        no attribute 'X'" even though the import succeeded.
        """
        import importlib

        module_names = [
            "commands.admin",
            "commands.advstats",
            "commands.betting",
            "commands.draft",
            "commands.enrichment",
            "commands.herogrid",
            "commands.info",
            "commands.lobby",
            "commands.match",
            "commands.predictions",
            "commands.profile",
            "commands.rating_analysis",
            "commands.registration",
            "commands.shop",
            "commands.wrapped",
        ]
        for name in module_names:
            module = importlib.import_module(name)
            assert module is not None, f"{name} imported as None"


