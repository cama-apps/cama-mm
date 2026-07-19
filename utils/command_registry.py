"""Discord application-command registration counts and limit warnings."""

from collections import Counter
from dataclasses import dataclass
from typing import Any

CHAT_INPUT_COMMAND_LIMIT = 100
COMMAND_OPTION_LIMIT = 25
COMMAND_OPTION_WARNING_THRESHOLD = 23


@dataclass(frozen=True)
class CommandRegistrationSummary:
    top_level_count: int
    node_count: int
    group_option_counts: dict[str, int]
    near_option_limit: dict[str, int]
    duplicate_qualified_names: tuple[str, ...]


def summarize_command_tree(tree: Any) -> CommandRegistrationSummary:
    """Return top-level and nested registration counts for a command tree."""
    top_level = list(tree.get_commands())
    nodes = list(tree.walk_commands())
    qualified_name_counts = Counter(command.qualified_name for command in nodes)
    group_option_counts = {
        command.qualified_name: len(command.commands)
        for command in nodes
        if getattr(command, "commands", None) is not None
    }

    return CommandRegistrationSummary(
        top_level_count=len(top_level),
        node_count=len(nodes),
        group_option_counts=group_option_counts,
        near_option_limit={
            name: count
            for name, count in group_option_counts.items()
            if count >= COMMAND_OPTION_WARNING_THRESHOLD
        },
        duplicate_qualified_names=tuple(
            sorted(name for name, count in qualified_name_counts.items() if count > 1)
        ),
    )
