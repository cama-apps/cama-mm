"""Tests for the Neon Degen Terminal easter egg system."""

import io

import pytest

from services.neon_degen_service import NeonDegenService, NeonResult
from utils.neon_terminal import (
    ansi_block,
    ascii_box,
    corrupt_text,
    render_balance_check,
    render_balance_zero,
    render_bankruptcy_filing,
    render_bet_placed,
    render_coinflip,
    render_cooldown_hit,
    render_debt_collector,
    render_don_lose,
    render_don_loss_box,
    render_don_win,
    render_loan_taken,
    render_match_recorded,
    render_negative_loan,
    render_prediction_market_crash,
    render_prediction_resolved,
    render_registration,
    render_soft_avoid,
    render_soft_avoid_surveillance,
    render_streak,
    render_system_breach,
    render_wheel_bankrupt,
)

# ---------------------------------------------------------------------------
# ASCII helpers (corrupt_text / ansi_block / ascii_box)
# ---------------------------------------------------------------------------


class TestNeonTerminalHelpers:
    def test_ansi_block_wraps_in_code_block(self):
        result = ansi_block("hello")
        assert result.startswith("```ansi\n")
        assert result.endswith("\n```")
        assert "hello" in result

    def test_ascii_box_creates_bordered_box(self):
        result = ascii_box(["line1", "line2"], width=20)
        assert "+" in result and "-" in result and "|" in result

    def test_corrupt_text_modifies_text_at_high_intensity(self):
        original = "abcdefghijklmnop"
        corrupted = corrupt_text(original, intensity=1.0)
        assert corrupted != original
        assert len(corrupted) == len(original)

    def test_corrupt_text_preserves_spaces(self):
        original = "hello world test"
        for _ in range(20):
            corrupted = corrupt_text(original, intensity=1.0)
            assert corrupted[5] == " "
            assert corrupted[11] == " "

    def test_corrupt_text_zero_intensity_is_passthrough(self):
        original = "hello world"
        assert corrupt_text(original, intensity=0.0) == original


# ---------------------------------------------------------------------------
# Render templates — happy path + signature keywords
# ---------------------------------------------------------------------------

# Renders that carry signature keywords. Anything not in this list is still
# covered by `test_all_renders_produce_ansi_block_under_45_lines` below.
RENDER_SIGNATURE_CASES = [
    (lambda: render_bankruptcy_filing("User", 300, 2), ["BANKRUPTCY"]),
    (lambda: render_debt_collector("User", 400), ["DEBT"]),
    (lambda: render_system_breach("User"), ["BREACH", "SYSTEM"]),
    (lambda: render_streak("User", 5, True), ["WIN", "STREAK", "HOT"]),
    (lambda: render_streak("User", 6, False), ["LOSS", "ANOMALY"]),
    (lambda: render_negative_loan("User", 50, -200), ["RECURSIVE", "DEBT"]),
    (lambda: render_don_loss_box("User", 100), ["DOUBLE", "NOTHING"]),
    (lambda: render_prediction_market_crash("Q?", 500, "yes", 3, 5), ["MARKET", "SETTLEMENT"]),
    (lambda: render_soft_avoid_surveillance(50, 3), ["SOCIAL", "Avoid"]),
]


@pytest.mark.parametrize("render_call,must_contain_any", RENDER_SIGNATURE_CASES)
def test_render_contains_signature_keyword(render_call, must_contain_any):
    result = render_call()
    assert "```ansi" in result
    assert any(kw in result for kw in must_contain_any), (
        f"Expected one of {must_contain_any} in render output"
    )


def test_all_renders_produce_ansi_block_under_45_lines():
    """Catch-all sanity: every render emits a code block and stays mobile-friendly."""
    renders = [
        render_balance_check("User", 100),
        render_balance_check("User", -200),
        render_bet_placed(50, "radiant", 1),
        render_bet_placed(50, "dire", 5),
        render_loan_taken(100, 120),
        render_cooldown_hit("loan"),
        render_match_recorded(),
        render_bankruptcy_filing("User", 300, 1),
        render_debt_collector("User", 400),
        render_system_breach("User"),
        render_balance_zero("User"),
        render_streak("User", 5, True),
        render_streak("User", 6, False),
        render_negative_loan("User", 50, -200),
        render_wheel_bankrupt("User", -100),
        render_don_win("User", 200),
        render_don_lose("User", 100),
        render_don_loss_box("User", 100),
        render_coinflip("Winner", "Loser"),
        render_registration("NewUser"),
        render_prediction_resolved("Test?", "yes", 200),
        render_prediction_market_crash("Test?", 500, "yes", 3, 5),
        render_soft_avoid(50, 3),
        render_soft_avoid_surveillance(50, 3),
    ]
    for render in renders:
        assert "```ansi" in render
        assert render.count("\n") <= 45, f"Render exceeds 45 lines:\n{render[:200]}"


# ---------------------------------------------------------------------------
# NeonDegenService orchestrator
# ---------------------------------------------------------------------------


class TestNeonDegenService:
    def _make_service(self) -> NeonDegenService:
        return NeonDegenService()

    @pytest.mark.asyncio
    async def test_on_balance_check_layer1_when_fires(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_balance_check(123, 456, 100)
            if result:
                assert result.layer == 1
                assert "```ansi" in result.text_block
                return
        pytest.fail("Expected balance_check to fire at least once in 100 tries")

    @pytest.mark.asyncio
    async def test_on_bankruptcy_always_fires(self):
        service = self._make_service()
        result = await service.on_bankruptcy(123, 456, debt_cleared=300, filing_number=2)
        assert result is not None
        assert result.layer >= 2

    @pytest.mark.asyncio
    @pytest.mark.parametrize("filing_number", [1, 3])
    async def test_on_bankruptcy_high_layer(self, filing_number):
        service = self._make_service()
        result = await service.on_bankruptcy(
            123, 456, debt_cleared=200, filing_number=filing_number
        )
        assert result is not None
        assert result.layer >= 2

    @pytest.mark.asyncio
    async def test_cooldown_prevents_rapid_fire(self):
        service = self._make_service()
        first = await service.on_bankruptcy(123, 456, debt_cleared=100, filing_number=2)
        assert first is not None
        for _ in range(10):
            assert await service.on_balance_check(123, 456, 100) is None

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "trigger,args",
        [
            ("on_balance_check", (123, 456, 100)),
            ("on_bankruptcy", (123, 456, 300, 5)),
            ("on_double_or_nothing", (123, 456, True, 100, 200)),
        ],
    )
    async def test_disabled_returns_none(self, trigger, args):
        import config

        original = config.NEON_DEGEN_ENABLED
        try:
            config.NEON_DEGEN_ENABLED = False
            service = self._make_service()
            method = getattr(service, trigger)
            assert await method(*args) is None
        finally:
            config.NEON_DEGEN_ENABLED = original

    @pytest.mark.asyncio
    async def test_on_bet_placed_fires_layer1(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_bet_placed(999, 456, 50, 1, "radiant")
            if result:
                assert result.layer == 1
                return
        pytest.fail("Expected bet_placed to fire at least once")

    @pytest.mark.asyncio
    @pytest.mark.parametrize("is_negative,expected_layer", [(False, 1), (True, 2)])
    async def test_on_loan_layer(self, is_negative, expected_layer):
        for _ in range(100):
            service = self._make_service()
            result = await service.on_loan(
                discord_id=777 + (1 if is_negative else 0),
                guild_id=456,
                amount=50,
                total_owed=200 if is_negative else 60,
                is_negative=is_negative,
            )
            if result and result.layer == expected_layer:
                return
        pytest.fail(
            f"Expected on_loan(is_negative={is_negative}) to produce layer {expected_layer}"
        )

    @pytest.mark.asyncio
    async def test_on_match_recorded_has_footer(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_match_recorded(456)
            if result:
                assert result.layer == 1
                assert result.footer_text is not None
                return
        pytest.fail("Expected match_recorded to fire at least once")

    @pytest.mark.asyncio
    async def test_on_degen_milestone_one_time(self):
        service = self._make_service()
        await service.on_degen_milestone(123, 456, 95)
        result2 = await service.on_degen_milestone(123, 456, 95)
        assert result2 is None

    def test_neon_result_dataclass_defaults(self):
        result = NeonResult(layer=1, text_block="test")
        assert result.layer == 1
        assert result.text_block == "test"
        assert result.gif_file is None
        assert result.footer_text is None

        buf = io.BytesIO(b"fake gif")
        result2 = NeonResult(layer=3, gif_file=buf, text_block="text")
        assert result2.layer == 3
        assert result2.gif_file is buf

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "won,balance_at_risk,final_balance",
        [(True, 100, 200), (False, 20, 0), (False, 80, 0)],
    )
    async def test_on_double_or_nothing_fires(self, won, balance_at_risk, final_balance):
        service = self._make_service()
        result = await service.on_double_or_nothing(
            123, 456, won=won, balance_at_risk=balance_at_risk, final_balance=final_balance
        )
        assert result is not None
        assert result.layer >= 1
        assert "```ansi" in result.text_block

    @pytest.mark.asyncio
    async def test_on_draft_coinflip_fires_layer1(self):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_draft_coinflip(456, 1001, 1002)
            if result:
                assert result.layer == 1
                return
        pytest.fail("Expected draft_coinflip to fire at least once")

    @pytest.mark.asyncio
    async def test_on_registration_one_time(self):
        service = self._make_service()
        fired = False
        for _ in range(100):
            result = await service.on_registration(126, 456, "NewPlayer")
            if result:
                fired = True
                assert result.layer == 1
                break
        if fired:
            for _ in range(10):
                assert await service.on_registration(126, 456, "NewPlayer") is None

    @pytest.mark.asyncio
    @pytest.mark.parametrize("total_pool", [50, 300])
    async def test_on_prediction_resolved_fires(self, total_pool):
        service = self._make_service()
        for _ in range(100):
            result = await service.on_prediction_resolved(
                guild_id=456,
                question="Q?",
                outcome="yes",
                total_pool=total_pool,
                winner_count=2,
                loser_count=3,
            )
            if result:
                assert result.layer >= 1
                return
        pytest.fail(f"Expected prediction_resolved (pool={total_pool}) to fire")

    @pytest.mark.asyncio
    async def test_on_soft_avoid_fires(self):
        for _ in range(100):
            service = self._make_service()
            result = await service.on_soft_avoid(888, 456, cost=50, games=3)
            if result:
                assert result.layer in (1, 2)
                assert "```ansi" in result.text_block
                return
        pytest.fail("Expected soft_avoid to fire at least once")


# ---------------------------------------------------------------------------
# GIF generation
# ---------------------------------------------------------------------------


GIF_CASES = [
    ("create_terminal_crash_gif", ("TestUser", 5)),
    ("create_void_welcome_gif", ("TestUser",)),
    ("create_debt_collector_gif", ("TestUser", 500)),
    ("create_freefall_gif", ("TestUser", 200, 0)),
    ("create_degen_certificate_gif", ("TestUser", 95)),
    ("create_don_coin_flip_gif", ("TestUser", 200)),
    ("create_market_crash_gif", (1000, "no", 5, 10)),
]


@pytest.mark.parametrize("fn_name,args", GIF_CASES)
def test_neon_gif_generates_under_4mb(fn_name, args):
    """Each GIF emits real GIF bytes and stays under Discord's 4MB upload limit."""
    import utils.neon_drawing as nd

    fn = getattr(nd, fn_name)
    buf = fn(*args)
    assert isinstance(buf, io.BytesIO)
    data = buf.getvalue()
    assert len(data) > 0
    assert data[:3] == b"GIF"
    size_mb = len(data) / (1024 * 1024)
    assert size_mb < 4, f"{fn_name} GIF is {size_mb:.2f} MB, exceeds 4MB limit"


# ---------------------------------------------------------------------------
# Persistence (neon_events table)
# ---------------------------------------------------------------------------


class TestNeonDegenPersistence:
    @pytest.mark.asyncio
    async def test_degen_milestone_persists_across_instances(self, repo_db_path):
        from repositories.neon_event_repository import NeonEventRepository
        from repositories.player_repository import PlayerRepository

        player_repo = PlayerRepository(repo_db_path)
        neon_event_repo = NeonEventRepository(repo_db_path)

        svc1 = NeonDegenService(player_repo=player_repo, neon_event_repo=neon_event_repo)
        result1 = await svc1.on_degen_milestone(123, 456, 95)
        assert result1 is not None

        neon_event_repo2 = NeonEventRepository(repo_db_path)
        svc2 = NeonDegenService(player_repo=player_repo, neon_event_repo=neon_event_repo2)
        assert await svc2.on_degen_milestone(123, 456, 95) is None

    @pytest.mark.asyncio
    async def test_one_time_db_fallback_without_repo(self):
        svc = NeonDegenService()
        assert await svc.on_degen_milestone(999, 456, 95) is not None
        assert await svc.on_degen_milestone(999, 456, 95) is None

        # New instance without repo - in-memory only, fires again
        svc2 = NeonDegenService()
        assert await svc2.on_degen_milestone(999, 456, 95) is not None

    @pytest.mark.asyncio
    async def test_different_triggers_independent(self, repo_db_path):
        from repositories.player_repository import PlayerRepository

        player_repo = PlayerRepository(repo_db_path)
        svc = NeonDegenService(player_repo=player_repo)

        assert await svc.on_degen_milestone(123, 456, 95) is not None
        assert svc._check_one_time(123, 456, "registration") is True
        assert svc._check_one_time(123, 456, "degen_90") is False

    @pytest.mark.asyncio
    async def test_different_guilds_independent(self, repo_db_path):
        from repositories.player_repository import PlayerRepository

        player_repo = PlayerRepository(repo_db_path)
        svc = NeonDegenService(player_repo=player_repo)

        assert await svc.on_degen_milestone(123, 456, 95) is not None
        assert await svc.on_degen_milestone(123, 789, 95) is not None
        assert await svc.on_degen_milestone(123, 456, 95) is None


# ---------------------------------------------------------------------------
# Privacy / anonymous mode
# ---------------------------------------------------------------------------


class TestNeonDegenPrivacy:
    """Sensitive events must not leak PII in public neon messages."""

    @pytest.mark.asyncio
    async def test_on_soft_avoid_neon_output_never_contains_buyer_name(self):
        from unittest.mock import MagicMock

        from domain.models.player import Player

        buyer_name = "SecretBuyer123"
        buyer_balance = 99999

        fake_player = Player(
            name=buyer_name,
            mmr=3000,
            initial_mmr=3000,
            preferred_roles=["1"],
            main_role="1",
            glicko_rating=1500.0,
            glicko_rd=200.0,
            glicko_volatility=0.06,
            os_mu=25.0,
            os_sigma=8.0,
            discord_id=888,
            jopacoin_balance=buyer_balance,
        )
        player_repo = MagicMock()
        player_repo.get_by_id = MagicMock(return_value=fake_player)

        for _ in range(200):
            svc = NeonDegenService(player_repo=player_repo)
            result = await svc.on_soft_avoid(888, 456, cost=50, games=3)
            if result and result.text_block:
                assert buyer_name not in result.text_block
                assert str(buyer_balance) not in result.text_block

    @pytest.mark.asyncio
    async def test_generate_text_anonymous_strips_player_context(self):
        from unittest.mock import AsyncMock

        ai_service = AsyncMock()
        ai_service.complete = AsyncMock(return_value=None)

        svc = NeonDegenService(ai_service=ai_service)
        fallback = "```ansi\nfallback text\n```"
        player_ctx = {"name": "LeakyName", "balance": 42}

        result = await svc._generate_text("some event", player_ctx, fallback, anonymous=True)
        assert result == fallback

        call_kwargs = ai_service.complete.call_args
        prompt_sent = call_kwargs.kwargs.get("prompt") or call_kwargs.args[0]
        system_sent = call_kwargs.kwargs.get("system_prompt", "")

        assert "LeakyName" not in prompt_sent
        assert "42" not in prompt_sent.split("Player context:")[1].split("Example output")[0]
        assert "ANONYMOUS" in system_sent
        assert "DO NOT include any player names" in system_sent

    @pytest.mark.asyncio
    async def test_generate_text_non_anonymous_includes_context(self):
        from unittest.mock import AsyncMock

        ai_service = AsyncMock()
        ai_service.complete = AsyncMock(return_value=None)

        svc = NeonDegenService(ai_service=ai_service)
        player_ctx = {"name": "VisiblePlayer", "balance": 100}
        await svc._generate_text("some event", player_ctx, "fallback")

        call_kwargs = ai_service.complete.call_args
        prompt_sent = call_kwargs.kwargs.get("prompt") or call_kwargs.args[0]
        system_sent = call_kwargs.kwargs.get("system_prompt", "")

        assert "VisiblePlayer" in prompt_sent
        assert "100" in prompt_sent
        assert "ANONYMOUS" not in system_sent


# ---------------------------------------------------------------------------
# on_match_enriched MVP compliments
# ---------------------------------------------------------------------------


class TestOnMatchEnriched:
    def _make_winner(
        self,
        discord_id=123,
        hero_id=1,
        kills=10,
        deaths=2,
        assists=15,
        gpm=600,
        fantasy=25.5,
    ):
        return {
            "discord_id": discord_id,
            "hero_id": hero_id,
            "kills": kills,
            "deaths": deaths,
            "assists": assists,
            "gpm": gpm,
            "fantasy_points": fantasy,
            "tower_damage": 5000,
            "hero_damage": 30000,
        }

    @pytest.mark.asyncio
    async def test_returns_empty_when_disabled(self, monkeypatch):
        import config

        monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", False)
        service = NeonDegenService()
        assert await service.on_match_enriched(0, [self._make_winner()]) == []

    @pytest.mark.asyncio
    async def test_returns_empty_when_no_winners(self):
        service = NeonDegenService()
        assert await service.on_match_enriched(0, []) == []

    @pytest.mark.asyncio
    async def test_returns_empty_when_all_rolls_fail(self, monkeypatch):
        import config
        import services.neon_degen_service as neon_mod

        monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
        monkeypatch.setattr(neon_mod, "NEON_MVP_CHANCE", 0.0)
        service = NeonDegenService()
        assert await service.on_match_enriched(0, [self._make_winner()]) == []

    @pytest.mark.asyncio
    async def test_returns_result_when_roll_succeeds(self, monkeypatch):
        import config
        import services.neon_degen_service as neon_mod

        monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
        monkeypatch.setattr(neon_mod, "NEON_MVP_CHANCE", 1.0)
        service = NeonDegenService()
        results = await service.on_match_enriched(0, [self._make_winner()])
        assert len(results) == 1
        assert results[0].layer == 2
        assert "```ansi" in results[0].text_block

    @pytest.mark.asyncio
    async def test_handles_missing_fields_gracefully(self, monkeypatch):
        import config
        import services.neon_degen_service as neon_mod

        monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
        monkeypatch.setattr(neon_mod, "NEON_MVP_CHANCE", 1.0)
        service = NeonDegenService()
        results = await service.on_match_enriched(0, [{"discord_id": 999}])
        assert len(results) == 1
        assert results[0].text_block is not None

    @pytest.mark.asyncio
    async def test_multiple_winners_independent_rolls(self, monkeypatch):
        import config
        import services.neon_degen_service as neon_mod

        monkeypatch.setattr(config, "NEON_DEGEN_ENABLED", True)
        monkeypatch.setattr(neon_mod, "NEON_MVP_CHANCE", 1.0)
        service = NeonDegenService()
        winners = [self._make_winner(discord_id=i) for i in range(5)]
        results = await service.on_match_enriched(0, winners)
        assert len(results) == 5
