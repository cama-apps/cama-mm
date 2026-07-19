"""Tests for the dig-native neon GIF moments and the big-win celebration.

Covers the animated GIF generators, the dig narrator persona roster, and the
service-level gating for the new hooks (rare/probabilistic, magnitude-scaled,
marquee-near-certain).
"""

import io
import random
from unittest.mock import AsyncMock

import pytest
from PIL import Image

import config
from services.dig_personas import DIG_VOICES, fallback_line, pick_dig_voice
from services.neon_degen_service import NeonDegenService, NeonResult
from utils import dig_drawing
from utils.neon_drawing import create_bigwin_gif


def _assert_valid_gif(buf, *, min_frames=2, max_mb=4):
    data = buf.getvalue()
    assert data, "GIF buffer is empty"
    assert len(data) < max_mb * 1024 * 1024, f"GIF too large: {len(data)} bytes"
    img = Image.open(io.BytesIO(data))
    assert img.format == "GIF"
    assert getattr(img, "n_frames", 1) >= min_frames


class TestDigGifGenerators:
    def test_reveal_victory(self):
        _assert_valid_gif(
            dig_drawing.animate_dig_reveal(
                "Magma", motion="victory", title="THE KING FALLS", sub_lines=("+240 jc",)
            )
        )

    def test_reveal_unearth(self):
        _assert_valid_gif(
            dig_drawing.animate_dig_reveal(
                "Crystal", motion="unearth", title="Crystal Compass", sprite_id="crystal"
            )
        )

    def test_legendary_relic(self):
        _assert_valid_gif(dig_drawing.animate_legendary_relic("Ethereal Crown"))

    def test_cave_in(self):
        _assert_valid_gif(dig_drawing.animate_cave_in("Stone", 112, 100))

    def test_pinnacle(self):
        _assert_valid_gif(dig_drawing.animate_pinnacle(prestige=False))

    def test_prestige(self):
        _assert_valid_gif(dig_drawing.animate_pinnacle(prestige=True))

    def test_unknown_layer_does_not_crash(self):
        # An unrecognised layer name must fall back to the Dirt palette, not raise.
        _assert_valid_gif(
            dig_drawing.animate_dig_reveal("Nonexistent Layer", motion="unearth", title="X")
        )

    @pytest.mark.parametrize("source", ["match", "prediction", "gamba"])
    @pytest.mark.parametrize("flavor", ["bigwin", "top_dog", "underdog"])
    def test_bigwin_gif(self, source, flavor):
        _assert_valid_gif(create_bigwin_gif("pf", 3200, source=source, flavor=flavor))


class TestDigPersonas:
    def test_affinity_bias(self):
        # cave_in-affinity voices (THE DAMP, A DROWNED MAP) should dominate the bias.
        rng = random.Random(0)
        picks = [pick_dig_voice("cave_in", rng).key for _ in range(200)]
        affinity = {"the_damp", "a_drowned_map"}
        assert sum(1 for k in picks if k in affinity) > 100

    def test_no_event_returns_valid_voice(self):
        assert pick_dig_voice(None, random.Random(1)).key in DIG_VOICES

    def test_fallback_line_deterministic_and_nonempty(self):
        a = fallback_line("legendary_relic", random.Random(5))
        b = fallback_line("legendary_relic", random.Random(5))
        assert a == b and isinstance(a, str) and a

    def test_fallback_line_unknown_event_uses_default(self):
        assert isinstance(fallback_line("does_not_exist", random.Random(2)), str)


@pytest.fixture
def enabled(monkeypatch):
    monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)


def _force_roll(service, value):
    service._roll = lambda chance: value


@pytest.mark.asyncio
class TestDigNeonHooks:
    async def test_dig_llm_env_switch_uses_static_caption(self):
        ai_service = AsyncMock()
        svc = NeonDegenService(
            ai_service=ai_service,
            dig_llm_enabled=False,
        )
        _force_roll(svc, True)

        caption = await svc._dig_caption(
            "boss_victory",
            "defeated a guardian",
            guild_id=0,
        )

        assert caption
        ai_service.complete.assert_not_awaited()

    async def test_boss_victory_fires_with_attribution(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r = await svc.on_dig_boss_victory(
            1, 0, boss_name="Burrow-King", boundary=100, layer_name="Magma", jc_delta=240
        )
        assert isinstance(r, NeonResult)
        assert r.layer == 3 and r.gif_file is not None
        assert r.text_block and "—" in r.text_block  # narrator voice attribution

    async def test_boss_victory_skipped_on_roll_miss(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, False)
        assert (
            await svc.on_dig_boss_victory(1, 0, boss_name="x", boundary=100, layer_name="Magma")
        ) is None

    async def test_disabled_returns_none(self, monkeypatch):
        monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", False)
        svc = NeonDegenService()
        _force_roll(svc, True)
        assert (
            await svc.on_dig_boss_victory(1, 0, boss_name="x", boundary=100, layer_name="Magma")
        ) is None

    async def test_legendary_relic_fires(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r = await svc.on_dig_relic_found(
            1, 0, relic_name="Ethereal Crown", rarity="legendary", layer_name="Abyss"
        )
        assert r and r.gif_file is not None

    async def test_common_relic_never_animates(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)  # forced — still must not animate a common drop
        assert (
            await svc.on_dig_relic_found(1, 0, relic_name="Pebble", rarity="common", layer_name="Dirt")
        ) is None

    async def test_cave_in_requires_rollback(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        assert (
            await svc.on_dig_cave_in(1, 0, depth_before=100, depth_after=100, layer_name="Stone")
        ) is None
        r = await svc.on_dig_cave_in(1, 0, depth_before=120, depth_after=100, layer_name="Stone")
        assert r and r.gif_file is not None

    async def test_prestige_fires(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r = await svc.on_dig_prestige(1, 0)
        assert r and r.gif_file is not None

    async def test_big_win_below_floor_returns_none(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        assert await svc.on_big_win(1, 0, source="match", payout=100) is None

    async def test_big_win_fires_above_floor(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r = await svc.on_big_win(1, 0, source="match", payout=3000, flavor="top_dog")
        assert r and r.gif_file is not None

    async def test_big_win_chance_scales_with_payout(self, enabled):
        svc = NeonDegenService()
        seen = []
        svc._roll = lambda chance: seen.append(chance) or False
        await svc.on_big_win(1, 0, source="gamba", payout=config.NEON_BIGWIN_MIN_PAYOUT)
        await svc.on_big_win(1, 0, source="gamba", payout=config.NEON_BIGWIN_FULL_PAYOUT * 3)
        assert seen[1] > seen[0]
        assert seen[1] <= 0.95 + 1e-9

    async def test_wheel_win_routes_to_big_win(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r = await svc.on_wheel_result(1, 0, result_value=4000, new_balance=5000)
        assert r and r.gif_file is not None

    async def test_don_big_win_routes_to_big_win(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r = await svc.on_double_or_nothing(1, 0, won=True, balance_at_risk=3000, final_balance=6000)
        assert r and r.gif_file is not None

    async def test_cooldown_blocks_second_fire(self, enabled):
        svc = NeonDegenService()
        _force_roll(svc, True)
        r1 = await svc.on_dig_boss_victory(1, 0, boss_name="x", boundary=100, layer_name="Magma")
        assert r1 is not None
        r2 = await svc.on_dig_relic_found(
            1, 0, relic_name="Crown", rarity="legendary", layer_name="Abyss"
        )
        assert r2 is None  # per-user cooldown set by the first fire
