import config
from commands.betting import _eruption_reward
from commands.betting_helpers.messages import WHEEL_EXPLOSION_REWARD
from commands.betting_helpers.wheel_embeds import build_wheel_result_embed
from services.dig_data.balance import strengthen_dig_event_penalty
from services.dig_splash import resolve_splash
from utils.economy_scaling import (
    DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER,
    adjust_generated_jc_reward,
    scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
)
from utils.wheel_drawing import GOLDEN_WHEEL_WEDGES, WHEEL_WEDGES


def test_scale_minigame_jc_delta_uses_half_up_rounding_and_preserves_sign():
    assert scale_minigame_jc_delta(0) == 0
    assert scale_minigame_jc_delta(1) == 1
    assert scale_minigame_jc_delta(2) == 2
    assert scale_minigame_jc_delta(3) == 3
    assert scale_minigame_jc_delta(5) == 5
    assert scale_minigame_jc_delta(100) == 100
    assert scale_minigame_jc_delta(-1) == -1
    assert scale_minigame_jc_delta(-3) == -3
    assert scale_minigame_jc_delta(-15) == -15


def test_scale_minigame_jc_delta_reads_configured_policy(monkeypatch):
    monkeypatch.setattr(config, "MINIGAME_JC_DELTA_SCALE", 0.5)

    assert scale_minigame_jc_delta(100) == 50
    assert scale_minigame_jc_delta(-15) == -8


def test_generated_reward_applies_central_scale_before_daily_policy():
    class _EventService:
        def __init__(self):
            self.received = None

        def adjust_reward(self, guild_id, amount):
            self.received = (guild_id, amount)
            return int(amount * 0.5)

    events = _EventService()

    assert adjust_generated_jc_reward(
        100,
        guild_id=123,
        economy_event_service=events,
    ) == 50
    assert events.received == (123, 100)


def test_generated_reward_skips_daily_reward_policy_for_losses():
    class _EventService:
        def adjust_reward(self, guild_id, amount):
            raise AssertionError("losses must use their surface-specific policy")

    assert adjust_generated_jc_reward(
        -100,
        guild_id=123,
        economy_event_service=_EventService(),
    ) == -100


def test_scale_deflationary_minigame_jc_delta_is_ten_percent_stronger():
    assert DEFLATIONARY_MINIGAME_JC_DELTA_MULTIPLIER == 1.10
    assert scale_deflationary_minigame_jc_delta(5) == 6
    assert scale_deflationary_minigame_jc_delta(10) == 11
    assert scale_deflationary_minigame_jc_delta(20) == 22
    assert scale_deflationary_minigame_jc_delta(-5) == -6
    assert scale_deflationary_minigame_jc_delta(-20) == -22
    assert scale_deflationary_minigame_jc_delta(20) > scale_minigame_jc_delta(20)


def test_wheel_numeric_wedges_are_scaled_for_display_and_payout():
    regular_values = {value for _label, value, _color in WHEEL_WEDGES}
    assert 5 in regular_values
    assert 100 in regular_values

    for label, value, _color in WHEEL_WEDGES:
        if isinstance(value, int) and value > 0:
            assert label == str(value)


def test_golden_wheel_numeric_wedges_are_scaled_for_display_and_payout():
    golden_values = {value for _label, value, _color in GOLDEN_WHEEL_WEDGES}
    assert 20 in golden_values
    assert 250 in golden_values

    for label, value, _color in GOLDEN_WHEEL_WEDGES:
        if isinstance(value, int) and value > 0:
            assert label == str(value)


def test_wheel_explosion_keeps_legacy_sixty_seven_reward():
    assert WHEEL_EXPLOSION_REWARD == 67


def test_emergency_embed_displays_scaled_loss_cap():
    embed = build_wheel_result_embed(
        ("EMERGENCY", "EMERGENCY", "#2a1a00"),
        new_balance=100,
        garnished=0,
        next_spin_time=0,
        emergency_count=3,
        emergency_total=48,
    )

    expected_cap = scale_minigame_jc_delta(20)
    assert f"up to **{expected_cap}**" in embed.description


def test_eruption_does_not_rescale_a_settled_spin():
    assert _eruption_reward({"result": 40}) == 80
    assert _eruption_reward({"result": -24}) == 48
    assert _eruption_reward(None) == scale_minigame_jc_delta(50)


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

    expected_penalty = scale_deflationary_minigame_jc_delta(
        strengthen_dig_event_penalty(10)
    )
    assert result.victims == [(3, expected_penalty)]
    assert player_repo.balances == {2: 49, 3: 50 - expected_penalty}
    assert player_repo.deltas == [(3, -expected_penalty)]
