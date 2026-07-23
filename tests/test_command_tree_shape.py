"""Static registration checks for Discord application-command limits and grouping."""

import ast
from pathlib import Path

COMMANDS_DIR = Path(__file__).resolve().parents[1] / "commands"


def _literal_keyword(call: ast.Call, name: str, default: str) -> str:
    for keyword in call.keywords:
        if keyword.arg == name:
            return ast.literal_eval(keyword.value)
    return default


def _registration_shape(path: Path) -> tuple[set[str], int, dict[str, int]]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    groups: dict[str, tuple[str, str | None]] = {}

    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign | ast.AnnAssign):
            continue
        targets = node.targets if isinstance(node, ast.Assign) else [node.target]
        value = node.value
        if not (
            isinstance(value, ast.Call)
            and isinstance(value.func, ast.Attribute)
            and value.func.attr == "Group"
        ):
            continue
        for target in targets:
            if not isinstance(target, ast.Name):
                continue
            parent_node = next(
                (keyword.value for keyword in value.keywords if keyword.arg == "parent"),
                None,
            )
            parent = parent_node.id if isinstance(parent_node, ast.Name) else None
            groups[target.id] = (
                _literal_keyword(value, "name", target.id),
                parent,
            )

    def group_path(group_id: str) -> list[str]:
        name, parent = groups[group_id]
        return [*group_path(parent), name] if parent else [name]

    command_paths: set[str] = set()
    direct_command_counts = dict.fromkeys(groups, 0)
    standalone_count = 0

    for node in ast.walk(tree):
        if not isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
            continue
        for decorator in node.decorator_list:
            if not (
                isinstance(decorator, ast.Call)
                and isinstance(decorator.func, ast.Attribute)
                and decorator.func.attr == "command"
                and isinstance(decorator.func.value, ast.Name)
            ):
                continue
            owner = decorator.func.value.id
            command_name = _literal_keyword(decorator, "name", node.name)
            if owner == "app_commands":
                standalone_count += 1
                command_paths.add(f"/{command_name}")
            elif owner in groups:
                direct_command_counts[owner] += 1
                command_paths.add(f"/{' '.join([*group_path(owner), command_name])}")

    top_level_groups = sum(parent is None for _, parent in groups.values())
    top_level_count = standalone_count + top_level_groups
    direct_option_counts: dict[str, int] = {}
    for group_id in groups:
        child_group_count = sum(parent == group_id for _, parent in groups.values())
        direct_option_counts[f"/{' '.join(group_path(group_id))}"] = (
            direct_command_counts[group_id] + child_group_count
        )

    return command_paths, top_level_count, direct_option_counts


def _all_registration_shapes() -> tuple[set[str], int, dict[str, int]]:
    command_paths: set[str] = set()
    top_level_count = 0
    direct_option_counts: dict[str, int] = {}
    for path in COMMANDS_DIR.glob("*.py"):
        module_paths, module_top_level, module_option_counts = _registration_shape(path)
        command_paths.update(module_paths)
        top_level_count += module_top_level
        direct_option_counts.update(module_option_counts)
    return command_paths, top_level_count, direct_option_counts


def test_commands_use_approved_consolidated_paths():
    command_paths, _, _ = _all_registration_shapes()

    expected_paths = {
        "/dig admin resetcooldown",
        "/dig admin forceevent",
        "/dig admin setdepth",
        "/economy tip",
        "/economy paydebt",
        "/economy bankruptcy",
        "/economy loan",
        "/economy reserve",
        "/economy disburse",
        "/shop buy",
        "/shop pingedash",
        "/shop pingedkevin",
        "/shop avoids",
        "/shop deals",
        "/shop mana",
        "/matches history",
        "/matches view",
        "/matches recent",
    }
    retired_paths = {
        "/dig resetcooldown",
        "/dig forceevent",
        "/dig setdepth",
        "/tip",
        "/paydebt",
        "/bankruptcy",
        "/loan",
        "/nonprofit",
        "/disburse",
        "/shop",
        "/pingedash",
        "/pingedkevin",
        "/myavoids",
        "/mydeals",
        "/manashop",
        "/matchhistory",
        "/viewmatch",
        "/recent",
    }

    assert expected_paths <= command_paths
    assert retired_paths.isdisjoint(command_paths)


def test_command_tree_stays_within_discord_limits():
    _, top_level_count, direct_option_counts = _all_registration_shapes()

    assert top_level_count == 41
    assert top_level_count <= 100
    assert all(count <= 25 for count in direct_option_counts.values())
    assert direct_option_counts["/dig"] == 22
