from utils.command_registry import summarize_command_tree


class _Command:
    def __init__(self, name: str, qualified_name: str):
        self.name = name
        self.qualified_name = qualified_name


class _Group(_Command):
    def __init__(self, name: str, qualified_name: str, option_count: int):
        super().__init__(name, qualified_name)
        self.commands = [object()] * option_count


class _Tree:
    def __init__(self, top_level: list[_Command], nodes: list[_Command]):
        self._top_level = top_level
        self._nodes = nodes

    def get_commands(self):
        return self._top_level

    def walk_commands(self):
        return self._nodes


def test_summary_separates_top_level_commands_from_all_nodes():
    dig = _Group("dig", "dig", 24)
    tree = _Tree(
        top_level=[dig, _Command("profile", "profile")],
        nodes=[
            dig,
            _Command("go", "dig go"),
            _Command("profile", "dig miner profile"),
            _Command("profile", "profile"),
        ],
    )

    summary = summarize_command_tree(tree)

    assert summary.top_level_count == 2
    assert summary.node_count == 4
    assert summary.group_option_counts == {"dig": 24}
    assert summary.near_option_limit == {"dig": 24}
    assert summary.duplicate_qualified_names == ()


def test_summary_detects_duplicate_qualified_paths_only():
    duplicate = _Command("go", "dig go")
    tree = _Tree(top_level=[], nodes=[duplicate, duplicate])

    summary = summarize_command_tree(tree)

    assert summary.duplicate_qualified_names == ("dig go",)
