"""
Tests for the /roll command (gambling math, balance flow, mana effects, edge cases).
"""

import random
import types

import pytest

from commands.roll import RollCommands
from domain.models.mana_effects import ManaEffects


class FakePlayer:
    """Lightweight player stand-in."""

    def __init__(self, discord_id: int = 42, balance: int = 100):
        self.discord_id = discord_id
        self.jopacoin_balance = balance
        self.name = f"Player{discord_id}"


class FakePlayerService:
    """In-memory player service used by /roll."""

    def __init__(self, *, balance: int = 100, registered: bool = True):
        self._registered = registered
        self.balance = balance
        self.adjustments: list[int] = []

    def get_player(self, discord_id, guild_id=None):
        if not self._registered:
            return None
        return FakePlayer(discord_id=discord_id, balance=self.balance)

    def get_balance(self, discord_id, guild_id=None):
        return self.balance

    def adjust_balance(self, discord_id, guild_id, delta):
        self.balance += delta
        self.adjustments.append(delta)
        return self.balance


class FakeManaEffectsService:
    """Stub mana effects service that returns a configured ManaEffects."""

    def __init__(self, effects: ManaEffects):
        self.effects = effects
        self.cashback_calls = 0
        self.tax_calls = 0
        self.tithe_calls = 0
        self.siphon_calls = 0
        self._cashback_amt = 0
        self._tax_amt = 0
        self._tithe_amt = 0
        self._siphon_payload: dict | None = None

    def get_effects(self, discord_id, guild_id):
        return self.effects

    def apply_green_cap(self, effects, gain):
        if effects.green_gain_cap is not None and gain > effects.green_gain_cap:
            return effects.green_gain_cap
        return gain

    def apply_blue_cashback(self, discord_id, guild_id, loss):
        self.cashback_calls += 1
        return self._cashback_amt

    def apply_blue_tax(self, discord_id, guild_id, gain):
        self.tax_calls += 1
        return self._tax_amt

    def apply_plains_tithe(self, discord_id, guild_id, gain):
        self.tithe_calls += 1
        return self._tithe_amt

    def execute_siphon(self, discord_id, guild_id):
        self.siphon_calls += 1
        return self._siphon_payload


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
            }
        )


class FakeResponse:
    def __init__(self):
        self.messages: list[dict] = []
        self._done = False

    async def send_message(self, content=None, ephemeral=None, embed=None, allowed_mentions=None):
        self._done = True
        self.messages.append({"content": content, "ephemeral": ephemeral, "embed": embed})

    async def defer(self, ephemeral=False):
        self._done = True

    def is_done(self):
        return self._done


class FakeMember:
    def __init__(self, member_id: int, *, bot: bool = False):
        self.id = member_id
        self.bot = bot
        self.mention = f"<@{member_id}>"


class FakeGuild:
    def __init__(self, *, guild_id: int = 12345, members: list[FakeMember] | None = None):
        self.id = guild_id
        self.members = members or []


class FakeInteraction:
    """Standalone interaction shim — does not depend on discord.py classes."""

    _next_id = 1000

    def __init__(self, *, user_id: int = 42, guild: FakeGuild | None = None):
        FakeInteraction._next_id += 1
        self.id = FakeInteraction._next_id
        self.user = types.SimpleNamespace(id=user_id, mention=f"<@{user_id}>")
        self.guild = guild if guild is not None else FakeGuild()
        self.response = FakeResponse()
        self.followup = FakeFollowup()
        self.channel = None


def make_cog(player_service, *, mana_effects_service=None):
    bot = types.SimpleNamespace()
    if mana_effects_service is not None:
        bot.mana_effects_service = mana_effects_service
    return RollCommands(bot, player_service)


@pytest.fixture(autouse=True)
def patch_safe_io(monkeypatch):
    """Replace defer with a no-op-ish AsyncMock that just marks the response done."""
    async def _safe_defer(interaction, ephemeral=False):
        interaction.response._done = True
        return True

    async def _safe_followup(interaction, **kw):
        await interaction.followup.send(**kw)

    monkeypatch.setattr("commands.roll.safe_defer", _safe_defer)
    monkeypatch.setattr("commands.roll.safe_followup", _safe_followup)


# ---------------------------------------------------------------------------
# Registration / input parsing
# ---------------------------------------------------------------------------


class TestRollRegistrationAndParsing:
    @pytest.mark.asyncio
    async def test_unregistered_player_blocked(self):
        svc = FakePlayerService(registered=False)
        cog = make_cog(svc)
        interaction = FakeInteraction()

        await cog.roll.callback(cog, interaction, "10")

        assert interaction.response.messages, "should have sent ephemeral error"
        msg = interaction.response.messages[0]
        assert msg["ephemeral"] is True
        assert "register" in msg["content"].lower()
        # No money moved
        assert svc.adjustments == []

    @pytest.mark.asyncio
    async def test_invalid_value_rejected(self):
        svc = FakePlayerService()
        cog = make_cog(svc)
        interaction = FakeInteraction()

        await cog.roll.callback(cog, interaction, "banana")

        assert interaction.response.messages
        msg = interaction.response.messages[0]
        assert msg["ephemeral"] is True
        assert "isn't a valid roll" in msg["content"]
        assert svc.adjustments == []

    @pytest.mark.asyncio
    async def test_zero_or_negative_rejected(self):
        svc = FakePlayerService()
        cog = make_cog(svc)
        interaction = FakeInteraction()

        await cog.roll.callback(cog, interaction, "0")

        assert interaction.response.messages
        assert interaction.response.messages[0]["ephemeral"] is True
        assert "positive integer" in interaction.response.messages[0]["content"]
        assert svc.adjustments == []


# ---------------------------------------------------------------------------
# Balance / cost handling
# ---------------------------------------------------------------------------


class TestRollBalanceGate:
    @pytest.mark.asyncio
    async def test_insufficient_balance_blocks_default(self):
        svc = FakePlayerService(balance=0)
        cog = make_cog(svc)
        interaction = FakeInteraction()

        await cog.roll.callback(cog, interaction, "50")

        assert interaction.response.messages
        msg = interaction.response.messages[0]
        assert msg["ephemeral"] is True
        assert "at least 1" in msg["content"]
        assert svc.adjustments == []

    @pytest.mark.asyncio
    async def test_insufficient_balance_against_red_cost(self):
        # Red mana raises roll cost to 2
        svc = FakePlayerService(balance=1)
        red = ManaEffects.for_color("Red", "Mountain")
        mes = FakeManaEffectsService(red)
        cog = make_cog(svc, mana_effects_service=mes)
        interaction = FakeInteraction()

        await cog.roll.callback(cog, interaction, "5")

        assert interaction.response.messages
        msg = interaction.response.messages[0]
        assert msg["ephemeral"] is True
        assert "at least 2" in msg["content"]
        assert svc.adjustments == []


# ---------------------------------------------------------------------------
# Sub-100 roll: deterministic deduction
# ---------------------------------------------------------------------------


class TestRollSub100:
    @pytest.mark.asyncio
    async def test_sub100_deducts_one_coin(self):
        svc = FakePlayerService(balance=10)
        cog = make_cog(svc)
        interaction = FakeInteraction()
        random.seed(1)

        await cog.roll.callback(cog, interaction, "20")

        # Always deducts roll_cost (1) for n < 100
        assert svc.adjustments == [-1]
        assert svc.balance == 9
        # The followup includes "rolled **N** (0–20)"
        assert interaction.followup.messages
        content = interaction.followup.messages[0]["content"]
        assert "(0–20)" in content
        assert "-1" in content
        assert "new balance: **9**" in content


# ---------------------------------------------------------------------------
# >=100 roll: jackpot path
# ---------------------------------------------------------------------------


class TestRollJackpot:
    @pytest.mark.asyncio
    async def test_jackpot_win_adds_jackpot_amount(self, monkeypatch):
        svc = FakePlayerService(balance=10)
        cog = make_cog(svc)
        interaction = FakeInteraction()

        # Force the jackpot path: first randint = display result, second = 0 to win.
        seq = iter([5, 0])
        monkeypatch.setattr("commands.roll.random.randint", lambda a, b: next(seq))

        await cog.roll.callback(cog, interaction, "100")

        # +20 jackpot, no -1 cost on win
        assert svc.adjustments == [20]
        assert svc.balance == 30
        content = interaction.followup.messages[0]["content"]
        assert "JACKPOT" in content
        assert "+20" in content

    @pytest.mark.asyncio
    async def test_jackpot_loss_deducts_one(self, monkeypatch):
        svc = FakePlayerService(balance=10)
        cog = make_cog(svc)
        interaction = FakeInteraction()

        # First randint = display result, second = 1 (not 0) → loss
        seq = iter([42, 1])
        monkeypatch.setattr("commands.roll.random.randint", lambda a, b: next(seq))

        await cog.roll.callback(cog, interaction, "100")

        assert svc.adjustments == [-1]
        assert svc.balance == 9
        content = interaction.followup.messages[0]["content"]
        assert "JACKPOT" not in content
        assert "-1" in content


# ---------------------------------------------------------------------------
# Mana effects on jackpot path
# ---------------------------------------------------------------------------


class TestRollManaEffects:
    @pytest.mark.asyncio
    async def test_red_jackpot_uses_red_amounts(self, monkeypatch):
        svc = FakePlayerService(balance=10)
        red = ManaEffects.for_color("Red", "Mountain")
        mes = FakeManaEffectsService(red)
        cog = make_cog(svc, mana_effects_service=mes)
        interaction = FakeInteraction()

        seq = iter([7, 0])  # display=7, win check=0 => jackpot
        monkeypatch.setattr("commands.roll.random.randint", lambda a, b: next(seq))

        await cog.roll.callback(cog, interaction, "100")

        # Red jackpot=40, plus default green_steady_bonus=0 (only Green sets +1)
        assert 40 in svc.adjustments
        # Net balance: +40 jackpot, with nothing else applied
        assert svc.balance == 50
        # No tax / tithe applied (Red has neither)
        assert mes.tax_calls == 1  # Called once but returned 0
        assert mes.tithe_calls == 1
        assert mes.cashback_calls == 0  # Not applied on win

    @pytest.mark.asyncio
    async def test_green_jackpot_caps_gain_and_adds_steady(self, monkeypatch):
        svc = FakePlayerService(balance=10)
        green = ManaEffects.for_color("Green", "Forest")
        mes = FakeManaEffectsService(green)
        cog = make_cog(svc, mana_effects_service=mes)
        interaction = FakeInteraction()

        seq = iter([99, 0])  # win
        monkeypatch.setattr("commands.roll.random.randint", lambda a, b: next(seq))

        await cog.roll.callback(cog, interaction, "100")

        # Default jackpot is 20, but we should call apply_green_cap with effects.
        # green_gain_cap is 50, jackpot is 20 (under cap). +1 steady bonus added.
        # Final win amount = 20 + 1 = 21
        assert 21 in svc.adjustments

    @pytest.mark.asyncio
    async def test_blue_loss_triggers_cashback(self, monkeypatch):
        svc = FakePlayerService(balance=10)
        blue = ManaEffects.for_color("Blue", "Island")
        mes = FakeManaEffectsService(blue)
        mes._cashback_amt = 1
        cog = make_cog(svc, mana_effects_service=mes)
        interaction = FakeInteraction()

        # Sub-100 always loses 1, triggering cashback
        random.seed(7)
        await cog.roll.callback(cog, interaction, "10")

        # roll_cost=1, then cashback=1
        assert -1 in svc.adjustments
        # Cashback was tried since we lost
        assert mes.cashback_calls == 1
        # Followup mentions cashback
        content = interaction.followup.messages[0]["content"]
        assert "Cashback" in content

    @pytest.mark.asyncio
    async def test_swamp_self_tax_applied(self, monkeypatch):
        svc = FakePlayerService(balance=20)
        black = ManaEffects.for_color("Black", "Swamp")
        mes = FakeManaEffectsService(black)
        # No siphon target available
        mes._siphon_payload = None
        cog = make_cog(svc, mana_effects_service=mes)
        interaction = FakeInteraction()

        random.seed(2)
        await cog.roll.callback(cog, interaction, "10")

        # Default loss -1, plus swamp self-tax -2
        assert -1 in svc.adjustments
        assert -2 in svc.adjustments
        content = interaction.followup.messages[0]["content"]
        assert "Swamp tax" in content


# ---------------------------------------------------------------------------
# Doggeh easter egg
# ---------------------------------------------------------------------------


class TestRollDoggeh:
    @pytest.mark.asyncio
    async def test_doggeh_costs_one_coin(self):
        svc = FakePlayerService(balance=5)
        cog = make_cog(svc)
        member = FakeMember(member_id=99)
        guild = FakeGuild(members=[member])
        interaction = FakeInteraction(user_id=42, guild=guild)
        random.seed(3)

        await cog.roll.callback(cog, interaction, "doggeh")

        assert svc.adjustments == [-1]
        # Followup contains the user's mention, "rolled **1**" and a prophecy
        content = interaction.followup.messages[0]["content"]
        assert "rolled **1**" in content
        assert "<@99>" in content  # the only valid target
        assert "new balance: **4**" in content

    @pytest.mark.asyncio
    async def test_doggeh_zero_balance_blocked(self):
        svc = FakePlayerService(balance=0)
        cog = make_cog(svc)
        interaction = FakeInteraction()

        await cog.roll.callback(cog, interaction, "doggeh")

        assert interaction.response.messages
        msg = interaction.response.messages[0]
        assert msg["ephemeral"] is True
        assert "demands 1" in msg["content"]
        assert svc.adjustments == []

    @pytest.mark.asyncio
    async def test_doggeh_falls_back_to_invoker_when_no_members(self):
        svc = FakePlayerService(balance=5)
        cog = make_cog(svc)
        # Guild with only bots (no real members)
        bot_member = FakeMember(member_id=999, bot=True)
        guild = FakeGuild(members=[bot_member])
        interaction = FakeInteraction(user_id=42, guild=guild)
        random.seed(0)

        await cog.roll.callback(cog, interaction, "doggeh")

        # No non-bot members present, should target the invoker
        content = interaction.followup.messages[0]["content"]
        assert "<@42>" in content
