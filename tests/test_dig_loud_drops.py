"""Boss-fight result embed surfaces gear drops and prestige relic drops
loudly so players know what they got."""

from commands.dig import _build_boss_fight_result_embed


class _FakeResult:
    """Minimal duck-typed stand-in for the dig service result object."""

    def __init__(self, **kwargs):
        self._d = kwargs
        for k, v in kwargs.items():
            setattr(self, k, v)


class TestBossFightResultEmbedDrops:
    def _base_kwargs(self, **overrides):
        defaults = {
            "won": True,
            "boss_name": "Grothak",
            "payout": 500,
            "jc_delta": 500,
            "win_chance": 0.6,
            "stat_point_awarded": False,
            "round_log": [],
            "extra_knockback": 0,
            "extra_cooldown_s": 0,
            "luminosity_display": None,
            "echo_applied": False,
            "gear_drop": None,
            "prestige_relic_drop": None,
        }
        defaults.update(overrides)
        return defaults

    def test_no_drop_omits_field(self):
        result = _FakeResult(**self._base_kwargs())
        embed = _build_boss_fight_result_embed(result=result, risk_tier="cautious", amount=10)
        names = [f.name for f in embed.fields]
        assert "Boss Drop" not in names
        assert "Relic Found" not in names

    def test_gear_drop_renders_field(self):
        result = _FakeResult(**self._base_kwargs(
            gear_drop={"gear_id": 1, "slot": "weapon", "tier": 6, "name": "Void-Touched Pickaxe"},
        ))
        embed = _build_boss_fight_result_embed(result=result, risk_tier="cautious", amount=10)
        gear_field = next((f for f in embed.fields if f.name == "Boss Drop"), None)
        assert gear_field is not None
        assert "Void-Touched Pickaxe" in gear_field.value
        assert "weapon" in gear_field.value

    def test_prestige_relic_drop_renders_field(self):
        result = _FakeResult(**self._base_kwargs(
            prestige_relic_drop={"id": "echo_stone", "name": "Echo Stone", "rarity": "Rare"},
        ))
        embed = _build_boss_fight_result_embed(result=result, risk_tier="cautious", amount=10)
        relic_field = next((f for f in embed.fields if f.name == "Relic Found"), None)
        assert relic_field is not None
        assert "Echo Stone" in relic_field.value

    def test_loss_does_not_render_drops(self):
        """Drops only render on victory — losing shouldn't show a drop field."""
        result = _FakeResult(**self._base_kwargs(
            won=False,
            gear_drop={"name": "Should not appear", "slot": "weapon", "tier": 1},
        ))
        embed = _build_boss_fight_result_embed(result=result, risk_tier="cautious", amount=10)
        names = [f.name for f in embed.fields]
        assert "Boss Drop" not in names
