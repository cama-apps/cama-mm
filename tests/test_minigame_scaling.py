from services.dig_splash import resolve_splash
from utils.economy_scaling import scale_minigame_jc_delta
from utils.wheel_drawing import GOLDEN_WHEEL_WEDGES, WHEEL_WEDGES


def test_scale_minigame_jc_delta_uses_half_up_rounding_and_preserves_sign():
    assert scale_minigame_jc_delta(0) == 0
    assert scale_minigame_jc_delta(1) == 1
    assert scale_minigame_jc_delta(2) == 2
    assert scale_minigame_jc_delta(3) == 2
    assert scale_minigame_jc_delta(5) == 4
    assert scale_minigame_jc_delta(100) == 80
    assert scale_minigame_jc_delta(-1) == -1
    assert scale_minigame_jc_delta(-3) == -2
    assert scale_minigame_jc_delta(-15) == -12


def test_wheel_numeric_wedges_are_scaled_for_display_and_payout():
    regular_values = {value for _label, value, _color in WHEEL_WEDGES}
    assert 4 in regular_values
    assert 80 in regular_values
    assert 5 not in regular_values
    assert 100 not in regular_values

    for label, value, _color in WHEEL_WEDGES:
        if isinstance(value, int) and value > 0:
            assert label == str(value)


def test_golden_wheel_numeric_wedges_are_scaled_for_display_and_payout():
    golden_values = {value for _label, value, _color in GOLDEN_WHEEL_WEDGES}
    assert 16 in golden_values
    assert 200 in golden_values
    assert 20 not in golden_values
    assert 250 not in golden_values

    for label, value, _color in GOLDEN_WHEEL_WEDGES:
        if isinstance(value, int) and value > 0:
            assert label == str(value)


class _FakePlayerRepo:
    def __init__(self):
        self.balances = {2: 49, 3: 50}
        self.deltas: list[tuple[int, int]] = []

    def get_all_registered_players_for_lottery(self, guild_id):
        assert guild_id == 123
        return [
            {"discord_id": 2},
            {"discord_id": 3},
        ]

    def get_balance(self, discord_id, guild_id):
        assert guild_id == 123
        return self.balances[discord_id]

    def add_balance(self, discord_id, guild_id, delta, **kwargs):
        assert guild_id == 123
        self.balances[discord_id] += delta
        self.deltas.append((discord_id, delta))


class _FakeDigRepo:
    def __init__(self):
        self.actions = []

    def log_action(self, **kwargs):
        self.actions.append(kwargs)


def test_dig_splash_burn_skips_players_below_auto_blind_threshold():
    player_repo = _FakePlayerRepo()
    dig_repo = _FakeDigRepo()

    result = resolve_splash(
        player_repo=player_repo,
        dig_repo=dig_repo,
        guild_id=123,
        digger_id=1,
        event_name="test_splash",
        strategy="random_active",
        victim_count=2,
        penalty_jc=10,
        mode="burn",
    )

    assert result.victims == [(3, 8)]
    assert player_repo.balances == {2: 49, 3: 42}
    assert player_repo.deltas == [(3, -8)]
