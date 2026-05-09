"""
Tests for the /ratinganalysis admin command (commands/rating_analysis.py).

These tests verify:
- Admin gating (non-admin invocations get rejected without side effects).
- Each action dispatches to the correct handler.
- The "player" sub-handler computes derived OpenSkill values correctly:
  * normalized_rating = mu * 50 + 250
  * is_calibrated when sigma <= 4.0
  * ordinal = mu - 3*sigma
- The "compare" sub-handler refuses gracefully when comparison data is missing.
- The "trend" sub-handler enforces the 20-match minimum.
- Backfill error path returns an embed-less ephemeral failure message.

Drawing functions are stubbed out so we don't generate real images.
"""

import types

import pytest

from commands.rating_analysis import RatingAnalysisCommands

# ---------------------------------------------------------------------------
# Discord interaction shims
# ---------------------------------------------------------------------------


class FakeFollowup:
    def __init__(self):
        self.messages: list[dict] = []

    async def send(
        self,
        content=None,
        embed=None,
        ephemeral=None,
        file=None,
        files=None,
        view=None,
        allowed_mentions=None,
    ):
        self.messages.append(
            {
                "content": content,
                "embed": embed,
                "ephemeral": ephemeral,
                "file": file,
            }
        )


class FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []
        self._done = False

    async def send_message(self, content=None, ephemeral=None, embed=None):
        self._done = True
        self.messages.append({"content": content, "ephemeral": ephemeral, "embed": embed})

    async def defer(self, ephemeral=False):
        self._done = True

    def is_done(self):
        return self._done


class FakeMember:
    def __init__(self, member_id: int, display_name: str = "Player"):
        self.id = member_id
        self.display_name = display_name
        self.mention = f"<@{member_id}>"
        self.bot = False


class FakeInteraction:
    _next_id = 9000

    def __init__(self, *, user_id: int = 1, guild_id: int | None = 12345):
        FakeInteraction._next_id += 1
        self.id = FakeInteraction._next_id
        self.user = FakeMember(user_id, "Admin")
        self.guild = types.SimpleNamespace(id=guild_id) if guild_id is not None else None
        self.response = FakeResponse()
        self.followup = FakeFollowup()
        self.channel = None


@pytest.fixture(autouse=True)
def patch_safe_io(monkeypatch):
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kw):
        await interaction.followup.send(**kw)

    monkeypatch.setattr("commands.rating_analysis.safe_defer", _safe_defer)
    monkeypatch.setattr("commands.rating_analysis.safe_followup", _safe_followup)


@pytest.fixture(autouse=True)
def stub_drawing(monkeypatch):
    """Drawing renders are matplotlib-heavy and expensive; we don't test them here."""
    import io

    def _fake_draw(*_a, **_kw):
        return io.BytesIO(b"\x89PNG\r\n\x1a\n" + b"x" * 16)

    monkeypatch.setattr("commands.rating_analysis.draw_calibration_curve", _fake_draw)
    monkeypatch.setattr("commands.rating_analysis.draw_prediction_over_time", _fake_draw)
    monkeypatch.setattr("commands.rating_analysis.draw_rating_comparison_chart", _fake_draw)


# ---------------------------------------------------------------------------
# Service stubs
# ---------------------------------------------------------------------------


class StubMatchService:
    def __init__(self, *, backfill_result=None, raise_backfill: Exception | None = None):
        self._backfill_result = backfill_result
        self._raise_backfill = raise_backfill
        self.history = []
        self.backfill_calls = 0

    def backfill_openskill_ratings(self, guild_id=None, reset_first=True):
        self.backfill_calls += 1
        if self._raise_backfill is not None:
            raise self._raise_backfill
        return self._backfill_result

    def get_player_openskill_history(self, discord_id, guild_id, limit=5):
        return self.history


class StubPlayerService:
    def __init__(self, players: dict | None = None, ratings: dict | None = None):
        self.players = players or {}
        self.ratings = ratings or {}

    def get_player(self, discord_id, guild_id=None):
        return self.players.get(discord_id)

    def get_openskill_rating(self, discord_id, guild_id=None):
        return self.ratings.get(discord_id)


class StubComparisonService:
    def __init__(self, summary=None, curve=None):
        self._summary = summary
        self._curve = curve
        self.summary_calls = 0
        self.curve_calls = 0

    def get_comparison_summary(self, guild_id=None):
        self.summary_calls += 1
        if isinstance(self._summary, Exception):
            raise self._summary
        return self._summary

    def get_calibration_curve_data(self, guild_id=None):
        self.curve_calls += 1
        if isinstance(self._curve, Exception):
            raise self._curve
        return self._curve


def _make_cog(*, match_service=None, player_service=None, comparison_service=None):
    bot = types.SimpleNamespace()
    return RatingAnalysisCommands(
        bot,
        match_service or StubMatchService(),
        player_service or StubPlayerService(),
        comparison_service,
    )


# ---------------------------------------------------------------------------
# Admin gate
# ---------------------------------------------------------------------------


class TestRatingAnalysisAdminGate:
    @pytest.mark.asyncio
    async def test_non_admin_blocked(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: False)
        cog = _make_cog()
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "compare", None)

        # Non-admin path uses interaction.response.send_message directly
        assert interaction.response.messages
        msg = interaction.response.messages[-1]
        assert msg["ephemeral"] is True
        assert "admin-only" in msg["content"].lower()

    @pytest.mark.asyncio
    async def test_unknown_action_rejected(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        cog = _make_cog()
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "bogus", None)

        assert interaction.response.messages
        msg = interaction.response.messages[-1]
        assert msg["ephemeral"] is True
        assert "Unknown action" in msg["content"]


# ---------------------------------------------------------------------------
# backfill action
# ---------------------------------------------------------------------------


class TestBackfillAction:
    @pytest.mark.asyncio
    async def test_backfill_success_renders_summary(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        match = StubMatchService(
            backfill_result={
                "matches_processed": 100,
                "total_matches": 100,
                "players_updated": 12,
                "errors": [],
            }
        )
        cog = _make_cog(match_service=match)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "backfill", None)

        assert match.backfill_calls == 1
        # Should have produced a starting message and then the embed result
        assert len(interaction.followup.messages) >= 2
        last = interaction.followup.messages[-1]
        embed = last["embed"]
        assert embed is not None
        assert embed.title == "OpenSkill Backfill Complete"
        # The "Matches Processed" field uses "100/100"
        names = {f.name: f.value for f in embed.fields}
        assert names.get("Matches Processed") == "100/100"
        assert names.get("Players Updated") == "12"

    @pytest.mark.asyncio
    async def test_backfill_includes_truncated_errors(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        errors = [f"err{i}" for i in range(8)]
        match = StubMatchService(
            backfill_result={
                "matches_processed": 50,
                "total_matches": 50,
                "players_updated": 3,
                "errors": errors,
            }
        )
        cog = _make_cog(match_service=match)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "backfill", None)

        embed = interaction.followup.messages[-1]["embed"]
        names = {f.name: f.value for f in embed.fields}
        assert "Errors" in names
        # Only first 5 plus the "and N more" suffix
        assert "err0" in names["Errors"]
        assert "err4" in names["Errors"]
        assert "err5" not in names["Errors"]
        assert "...and 3 more" in names["Errors"]

    @pytest.mark.asyncio
    async def test_backfill_exception_returns_message(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        match = StubMatchService(raise_backfill=RuntimeError("DB exploded"))
        cog = _make_cog(match_service=match)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "backfill", None)

        # Should produce two messages: the "starting…" notice and the error.
        assert any("Backfill failed" in (m["content"] or "") for m in interaction.followup.messages)
        # Only a content message; no embed in the failure path
        last_failure = next(
            m for m in reversed(interaction.followup.messages) if m["content"] and "Backfill failed" in m["content"]
        )
        assert last_failure["embed"] is None


# ---------------------------------------------------------------------------
# compare action
# ---------------------------------------------------------------------------


class TestCompareAction:
    def _summary_payload(self, *, brier_g, brier_o, acc_g, acc_o):
        return {
            "matches_analyzed": 100,
            "glicko": {
                "brier_score": brier_g,
                "accuracy": acc_g,
                "log_loss": 0.65,
                "calibration": {},
            },
            "openskill": {
                "brier_score": brier_o,
                "accuracy": acc_o,
                "log_loss": 0.66,
                "calibration": {},
            },
            "comparison": {
                "brier_winner": "Glicko-2" if brier_g < brier_o else "OpenSkill",
                "brier_difference": abs(brier_g - brier_o),
                "accuracy_winner": "Glicko-2" if acc_g > acc_o else "OpenSkill",
                "accuracy_difference": abs(acc_g - acc_o),
            },
            "match_data": [],
        }

    @pytest.mark.asyncio
    async def test_compare_no_service(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        cog = _make_cog(comparison_service=None)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "compare", None)

        # Without a comparison service, a plain message is sent
        msg = interaction.followup.messages[-1]
        assert msg["embed"] is None
        assert "not available" in msg["content"]

    @pytest.mark.asyncio
    async def test_compare_error_payload_handled(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        cmp = StubComparisonService(summary={"error": "Insufficient data", "matches_analyzed": 0})
        cog = _make_cog(comparison_service=cmp)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "compare", None)

        msg = interaction.followup.messages[-1]
        assert msg["embed"] is None
        assert "Insufficient data" in msg["content"]

    @pytest.mark.asyncio
    async def test_compare_renders_summary_and_chart(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        # Glicko clearly better on both metrics
        summary = self._summary_payload(brier_g=0.20, brier_o=0.24, acc_g=0.65, acc_o=0.55)
        cmp = StubComparisonService(summary=summary)
        cog = _make_cog(comparison_service=cmp)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "compare", None)

        # Embed posted with chart attachment
        msg = interaction.followup.messages[-1]
        embed = msg["embed"]
        assert embed is not None
        assert embed.title == "Rating System Comparison"
        # Summary field should announce the winner
        summary_field = next(f for f in embed.fields if f.name == "Summary")
        assert "Glicko-2" in summary_field.value
        # File attachment was included
        assert msg["file"] is not None

    @pytest.mark.asyncio
    async def test_compare_marginal_difference_summary(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        # Difference < 5%
        summary = self._summary_payload(brier_g=0.200, brier_o=0.201, acc_g=0.55, acc_o=0.54)
        cog = _make_cog(comparison_service=StubComparisonService(summary=summary))
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "compare", None)

        embed = interaction.followup.messages[-1]["embed"]
        summary_field = next(f for f in embed.fields if f.name == "Summary")
        assert "marginal" in summary_field.value.lower()


# ---------------------------------------------------------------------------
# calibration action
# ---------------------------------------------------------------------------


class TestCalibrationAction:
    @pytest.mark.asyncio
    async def test_calibration_no_service(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        cog = _make_cog(comparison_service=None)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "calibration", None)

        msg = interaction.followup.messages[-1]
        assert msg["embed"] is None
        assert "not available" in msg["content"]

    @pytest.mark.asyncio
    async def test_calibration_error_payload(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        cmp = StubComparisonService(curve={"error": "Insufficient data"})
        cog = _make_cog(comparison_service=cmp)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "calibration", None)

        msg = interaction.followup.messages[-1]
        assert "Insufficient data" in msg["content"]

    @pytest.mark.asyncio
    async def test_calibration_renders_chart(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        curve = {
            "glicko": [(0.55, 0.6, 10)],
            "openskill": [(0.45, 0.4, 10)],
            "perfect_line": [(0.0, 0.0), (1.0, 1.0)],
        }
        cmp = StubComparisonService(curve=curve)
        cog = _make_cog(comparison_service=cmp)
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "calibration", None)

        msg = interaction.followup.messages[-1]
        embed = msg["embed"]
        assert embed is not None
        assert embed.title == "Rating System Calibration"
        assert msg["file"] is not None


# ---------------------------------------------------------------------------
# trend action
# ---------------------------------------------------------------------------


class TestTrendAction:
    @pytest.mark.asyncio
    async def test_trend_requires_at_least_20_matches(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        summary = {
            "matches_analyzed": 19,
            "glicko": {"brier_score": 0.22, "accuracy": 0.55, "log_loss": 0.6, "calibration": {}},
            "openskill": {"brier_score": 0.23, "accuracy": 0.54, "log_loss": 0.61, "calibration": {}},
            "comparison": {
                "brier_winner": "Glicko-2",
                "brier_difference": 0.01,
                "accuracy_winner": "Glicko-2",
                "accuracy_difference": 0.01,
            },
            "match_data": [{"match_id": i} for i in range(19)],  # < 20
        }
        cog = _make_cog(comparison_service=StubComparisonService(summary=summary))
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "trend", None)

        msg = interaction.followup.messages[-1]
        assert msg["embed"] is None
        assert "20 matches" in msg["content"]

    @pytest.mark.asyncio
    async def test_trend_renders_chart_with_enough_matches(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        summary = {
            "matches_analyzed": 25,
            "glicko": {"brier_score": 0.22, "accuracy": 0.55, "log_loss": 0.6, "calibration": {}},
            "openskill": {"brier_score": 0.23, "accuracy": 0.54, "log_loss": 0.61, "calibration": {}},
            "comparison": {
                "brier_winner": "Glicko-2",
                "brier_difference": 0.01,
                "accuracy_winner": "Glicko-2",
                "accuracy_difference": 0.01,
            },
            "match_data": [{"match_id": i} for i in range(25)],
        }
        cog = _make_cog(comparison_service=StubComparisonService(summary=summary))
        interaction = FakeInteraction()

        await cog.ratinganalysis.callback(cog, interaction, "trend", None)

        msg = interaction.followup.messages[-1]
        embed = msg["embed"]
        assert embed is not None
        assert embed.title == "Prediction Accuracy Over Time"
        assert msg["file"] is not None


# ---------------------------------------------------------------------------
# player action — the only one with substantive math worth verifying.
# ---------------------------------------------------------------------------


class TestPlayerAction:
    @pytest.mark.asyncio
    async def test_unregistered_target(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        cog = _make_cog(player_service=StubPlayerService(players={}))
        interaction = FakeInteraction()
        target = FakeMember(123, "Ghost")

        await cog.ratinganalysis.callback(cog, interaction, "player", target)

        msg = interaction.followup.messages[-1]
        assert "not registered" in msg["content"]
        assert "Ghost" in msg["content"]

    @pytest.mark.asyncio
    async def test_player_with_openskill_data_calibrated(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        # mu=30, sigma=3.0 → calibrated (sigma <= 4.0)
        # ordinal = 30 - 9 = 21.0; normalized = 30 * 50 + 250 = 1750
        player_obj = types.SimpleNamespace(glicko_rating=1500.0, glicko_rd=80.0)
        ps = StubPlayerService(
            players={42: player_obj},
            ratings={42: (30.0, 3.0)},
        )
        match_svc = StubMatchService()
        match_svc.history = [
            {
                "os_mu_before": 25.0,
                "os_mu_after": 26.5,
                "os_sigma_before": 5.0,
                "os_sigma_after": 4.5,
                "won": True,
                "fantasy_weight": 1.5,
            }
        ]
        cog = _make_cog(match_service=match_svc, player_service=ps)
        interaction = FakeInteraction()
        target = FakeMember(42, "Star")

        await cog.ratinganalysis.callback(cog, interaction, "player", target)

        embed = interaction.followup.messages[-1]["embed"]
        assert embed is not None
        assert "Star" in embed.title
        names = {f.name: f.value for f in embed.fields}
        # Normalized rating: 30 * 50 + 250 = 1750
        assert "1750" in names["Normalized Rating"]
        # Glicko comparison present
        assert "1500" in names["Glicko-2 Rating"]
        # Calibrated since sigma <= 4.0
        assert "Yes" in names["Calibrated"]
        # μ field shows 30.00
        assert "30.00" in names["Skill (μ)"]
        # σ field shows 3.000
        assert "3.000" in names["Uncertainty (σ)"]
        # Ordinal: 30 - 3*3 = 21.00
        assert "21.00" in names["Ordinal (μ-3σ)"]
        # History rendered
        history_field = names["Recent OpenSkill Changes"]
        assert "+1.50" in history_field  # mu change
        assert "(w=1.5)" in history_field

    @pytest.mark.asyncio
    async def test_player_with_high_sigma_not_calibrated(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        # sigma > 4.0 → not calibrated
        player_obj = types.SimpleNamespace(glicko_rating=1400.0, glicko_rd=200.0)
        ps = StubPlayerService(
            players={9: player_obj},
            ratings={9: (20.0, 6.0)},
        )
        cog = _make_cog(player_service=ps)
        interaction = FakeInteraction()
        target = FakeMember(9, "Newbie")

        await cog.ratinganalysis.callback(cog, interaction, "player", target)

        embed = interaction.followup.messages[-1]["embed"]
        names = {f.name: f.value for f in embed.fields}
        # Not calibrated text
        assert "No" in names["Calibrated"]
        assert "4.0" in names["Calibrated"]

    @pytest.mark.asyncio
    async def test_player_without_openskill_shows_help_message(self, monkeypatch):
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        player_obj = types.SimpleNamespace(glicko_rating=None, glicko_rd=None)
        ps = StubPlayerService(
            players={5: player_obj},
            ratings={5: None},
        )
        cog = _make_cog(player_service=ps)
        interaction = FakeInteraction()
        target = FakeMember(5, "Empty")

        await cog.ratinganalysis.callback(cog, interaction, "player", target)

        embed = interaction.followup.messages[-1]["embed"]
        # No rating fields, but the description explains how to populate.
        assert embed is not None
        assert "OpenSkill" in embed.description
        assert "backfill" in embed.description.lower()

    @pytest.mark.asyncio
    async def test_player_defaults_to_invoker(self, monkeypatch):
        """When no user is supplied, the command targets the invoker."""
        monkeypatch.setattr("commands.rating_analysis.has_admin_permission", lambda _: True)
        # Set up so user_id == 1 has data
        player_obj = types.SimpleNamespace(glicko_rating=1500.0, glicko_rd=80.0)
        ps = StubPlayerService(
            players={1: player_obj},
            ratings={1: (25.0, 3.5)},
        )
        cog = _make_cog(player_service=ps)
        interaction = FakeInteraction(user_id=1)

        await cog.ratinganalysis.callback(cog, interaction, "player", None)

        embed = interaction.followup.messages[-1]["embed"]
        # Title should pick up the invoker's display_name
        assert "Admin" in embed.title  # FakeInteraction sets user.display_name="Admin"
