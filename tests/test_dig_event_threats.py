"""Engine tests for the /dig event "threat" downsides.

Covers the streak-loss and curse threats applied by a failed risky event
choice, plus the JC threat's no-floor debt behaviour:

* the partial, scaled streak setback (never a full reset),
* a curse applied via ``resolve_event`` that persists, slows a later dig,
  decrements per dig, and expires after ``duration_digs``,
* a large negative-JC outcome that can push a balance below zero.

Events are constructed synthetically and injected into ``EVENT_POOL`` for
the duration of a test — the real event catalog is being re-tuned in a
separate effort, so nothing here asserts on real catalog numbers.
"""

import random
import time

import pytest

import services.dig_service as dig_service_module
from repositories.dig_repository import DigRepository
from services.dig_constants import FREE_DIG_COOLDOWN_SECONDS
from services.dig_service import DigService


@pytest.fixture
def dig_repo(repo_db_path):
    return DigRepository(repo_db_path)


@pytest.fixture
def dig_service(dig_repo, player_repository, monkeypatch):
    svc = DigService(dig_repo, player_repository)
    # Neutralize weather so its random rolls can't swamp the threat effects.
    monkeypatch.setattr(svc, "_get_weather_effects", lambda guild_id, layer_name: {})
    return svc


def _register_player(player_repository, discord_id=10001, guild_id=12345, balance=500):
    """Register a player with a known balance."""
    player_repository.add(
        discord_id=discord_id,
        discord_username=f"Player{discord_id}",
        guild_id=guild_id,
        initial_mmr=3000,
        glicko_rating=1500.0,
        glicko_rd=350.0,
        glicko_volatility=0.06,
    )
    player_repository.update_balance(discord_id, guild_id, balance)
    return discord_id


def _outcome(description="", advance=0, jc=0, cave_in=False, streak_loss=0, curse=None):
    """Build an EventOutcome dict in the EVENT_POOL shape."""
    return {
        "description": description,
        "advance": advance,
        "jc": jc,
        "cave_in": cave_in,
        "streak_loss": streak_loss,
        "curse": curse,
    }


def _synthetic_event(event_id, *, risky_success, risky_failure, risky_chance=0.5):
    """Build a minimal two-branch event dict and register it in EVENT_POOL.

    Returns the event dict; the caller is responsible for nothing — a
    fixture-scoped cleanup is wired by ``injected_event``.
    """
    return {
        "id": event_id,
        "name": f"Synthetic {event_id}",
        "description": ("A test passage.",),
        "min_depth": 0,
        "max_depth": None,
        "safe_option": {
            "label": "Safe",
            "success": _outcome("You play it safe.", jc=2),
            "failure": None,
            "success_chance": 1.0,
        },
        "risky_option": {
            "label": "Risky",
            "success": risky_success,
            "failure": risky_failure,
            "success_chance": risky_chance,
        },
        "complexity": "choice",
        "layer": None,
        "rarity": "common",
        "requires_dark": False,
        "social": False,
        "ascii_art": None,
        "buff_on_success": None,
        "desperate_option": None,
        "boon_options": None,
        "min_prestige": 0,
        "next_event_id": None,
        "chain_only": False,
        "splash": None,
        "guild_modifier_on_success": None,
        "quest_id": None,
        "quest_step": None,
    }


@pytest.fixture
def inject_event():
    """Append a synthetic event to EVENT_POOL and tear it down after."""
    added: list[str] = []

    def _add(event: dict) -> dict:
        dig_service_module.EVENT_POOL.append(event)
        added.append(event["id"])
        return event

    yield _add

    dig_service_module.EVENT_POOL[:] = [
        e for e in dig_service_module.EVENT_POOL if e["id"] not in added
    ]


def _start_tunnel(dig_service, dig_repo, discord_id, guild_id, monkeypatch, *, depth=50):
    """Create a tunnel for ``discord_id`` and park it at ``depth``."""
    monkeypatch.setattr(time, "time", lambda: 1_000_000)
    monkeypatch.setattr(random, "random", lambda: 0.99)  # suppress events on the seed dig
    dig_service.dig(discord_id, guild_id)
    dig_repo.update_tunnel(discord_id, guild_id, depth=depth)


# ---------------------------------------------------------------------------
# Streak threat — partial, scaled setback
# ---------------------------------------------------------------------------


class TestStreakThreat:
    """A failed risky pick on a streak-themed event knocks days off the
    daily streak — partially, scaled by streak length, never to zero."""

    def _resolve_streak_failure(
        self, dig_service, dig_repo, player_repository, monkeypatch,
        inject_event, *, streak_days, streak_loss,
        discord_id=10001, guild_id=12345,
    ):
        _register_player(player_repository, discord_id, guild_id)
        _start_tunnel(dig_service, dig_repo, discord_id, guild_id, monkeypatch)
        dig_repo.update_tunnel(discord_id, guild_id, streak_days=streak_days)

        event = _synthetic_event(
            f"threat_streak_{streak_days}_{streak_loss}",
            risky_success=_outcome("You held the line.", jc=20),
            risky_failure=_outcome("The dark eats your momentum.", streak_loss=streak_loss),
        )
        inject_event(event)

        # Force the risky choice to FAIL (roll >= success_chance of 0.5).
        monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.99)
        return dig_service.resolve_event(discord_id, guild_id, event["id"], "risky")

    def test_streak_setback_applies_and_is_partial(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A failed pick reduces the streak by the base loss — but does not
        reset it: a 10-day streak losing base 3 lands at 7, not 0/1."""
        result = self._resolve_streak_failure(
            dig_service, dig_repo, player_repository, monkeypatch, inject_event,
            streak_days=10, streak_loss=3,
        )
        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, 12345)
        # base 3 + floor(10/20)=0 -> setback 3 -> 10-3 = 7
        assert tunnel["streak_days"] == 7
        assert result.get("streak_loss") == 3

    def test_streak_setback_scales_with_streak_length(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """The setback grows +1 per 20 streak days, so a long streak loses
        strictly more than a short one for the same base loss."""
        result = self._resolve_streak_failure(
            dig_service, dig_repo, player_repository, monkeypatch, inject_event,
            streak_days=45, streak_loss=3,
        )
        tunnel = dig_repo.get_tunnel(10001, 12345)
        # base 3 + floor(45/20)=2 -> setback 5 -> 45-5 = 40
        assert tunnel["streak_days"] == 40
        assert result.get("streak_loss") == 5

    def test_streak_setback_never_resets_to_zero(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A short streak hit by a big base loss floors at 0 but the setback
        is still a clamped subtraction, not a hard reset to 0/1 unprovoked:
        a 2-day streak losing base 3 (+0 scale) lands at 0."""
        result = self._resolve_streak_failure(
            dig_service, dig_repo, player_repository, monkeypatch, inject_event,
            streak_days=2, streak_loss=3,
        )
        tunnel = dig_repo.get_tunnel(10001, 12345)
        assert tunnel["streak_days"] == 0  # max(0, 2-3)
        # Only the 2 days actually held were lost — not a phantom 3.
        assert result.get("streak_loss") == 2

    def test_streak_setback_leaves_long_streak_buffer(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A 30+ day streak keeps most of its buffer — the setback only
        nicks it, preserving the bonus tier the player has banked."""
        result = self._resolve_streak_failure(
            dig_service, dig_repo, player_repository, monkeypatch, inject_event,
            streak_days=60, streak_loss=2,
        )
        tunnel = dig_repo.get_tunnel(10001, 12345)
        # base 2 + floor(60/20)=3 -> setback 5 -> 60-5 = 55
        assert tunnel["streak_days"] == 55
        assert result.get("streak_loss") == 5

    def test_risky_success_leaves_streak_untouched(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """The streak threat only fires on FAILURE — a successful risky pick
        on the same event must not touch the streak."""
        _register_player(player_repository)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch)
        dig_repo.update_tunnel(10001, 12345, streak_days=10)

        event = _synthetic_event(
            "threat_streak_success",
            risky_success=_outcome("You held the line.", jc=20),
            risky_failure=_outcome("Momentum lost.", streak_loss=5),
        )
        inject_event(event)
        # Force SUCCESS (roll < 0.5).
        monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.01)
        result = dig_service.resolve_event(10001, 12345, event["id"], "risky")

        assert result["success"]
        tunnel = dig_repo.get_tunnel(10001, 12345)
        assert tunnel["streak_days"] == 10
        assert result.get("streak_loss") == 0


# ---------------------------------------------------------------------------
# Curse threat — applied, persisted, drains a dig, decrements, expires
# ---------------------------------------------------------------------------


class TestCurseThreat:
    """A failed risky pick on a hex-themed event applies a lingering curse
    that drains subsequent digs and expires after ``duration_digs``."""

    SLOW_CURSE = {
        "id": "test_slow_hex",
        "name": "Slowing Hex",
        "duration_digs": 2,
        "effect": {"advance_bonus": -3},
    }

    def _apply_curse_via_event(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
        curse, discord_id=10001, guild_id=12345,
    ):
        _register_player(player_repository, discord_id, guild_id)
        _start_tunnel(dig_service, dig_repo, discord_id, guild_id, monkeypatch)

        event = _synthetic_event(
            f"threat_curse_{curse['id']}",
            risky_success=_outcome("The idol stays quiet.", jc=20),
            risky_failure=_outcome("The idol's eyes open.", curse=curse),
        )
        inject_event(event)
        monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.99)
        return dig_service.resolve_event(discord_id, guild_id, event["id"], "risky")

    def test_curse_applied_and_persisted(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A failed risky pick writes the curse to the temp_curses column,
        and ``_get_active_curse`` reads it back with the right shape."""
        result = self._apply_curse_via_event(
            dig_service, dig_repo, player_repository, monkeypatch, inject_event,
            self.SLOW_CURSE,
        )
        assert result["success"]
        curse_applied = result.get("curse_applied")
        assert curse_applied is not None
        assert curse_applied["id"] == "test_slow_hex"

        tunnel = dig_repo.get_tunnel(10001, 12345)
        curse = dig_service._get_active_curse(dict(tunnel))
        assert curse is not None
        assert curse["id"] == "test_slow_hex"
        assert curse["digs_remaining"] == 2
        assert curse["effect"] == {"advance_bonus": -3}

    def test_curse_does_not_clobber_active_buff(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """The curse lands in its own column — a buff active at the same
        time survives the curse being applied."""
        self._apply_curse_via_event(
            dig_service, dig_repo, player_repository, monkeypatch, inject_event,
            self.SLOW_CURSE,
        )
        dig_service.set_temp_buff(10001, 12345, {
            "id": "power", "name": "Power", "duration_digs": 3,
            "effect": {"advance_bonus": 5},
        })
        tunnel = dict(dig_repo.get_tunnel(10001, 12345))
        assert dig_service._get_active_buff(tunnel) is not None
        assert dig_service._get_active_curse(tunnel) is not None

    def test_curse_advance_drain_reduces_a_dig(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """An active advance-draining curse makes the next dig advance less
        than the same dig would without the curse."""
        _register_player(player_repository, 10001)
        _register_player(player_repository, 10002)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch, depth=20)
        _start_tunnel(dig_service, dig_repo, 10002, 12345, monkeypatch, depth=20)

        # Curse one player with an advance drain; leave the other clean. A
        # -1 drain stays above the engine's max(1, advance) floor on a Dirt
        # dig, so the full effect is observable.
        dig_service.set_temp_curse(10001, 12345, {
            "id": "test_slow", "name": "Slowing Hex", "duration_digs": 3,
            "effect": {"advance_bonus": -1},
        })

        # Deterministic dig: max base advance roll, no events, past cooldown.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(random, "randint", lambda a, b: b)

        cursed = dig_service.dig(10001, 12345)
        clean = dig_service.dig(10002, 12345)
        assert cursed["success"] and clean["success"]
        # Same roll, same depth/layer — the curse strips 1 off the advance.
        assert cursed["advance"] == clean["advance"] - 1
        assert cursed["advance"] < clean["advance"]

    def test_curse_jc_drain_reduces_earnings_floored_at_zero(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A JC-draining curse cuts a dig's earnings; the existing max(0,...)
        clamp still floors the dig at 0 — a curse never makes a dig cost JC."""
        _register_player(player_repository, 10001, balance=500)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch, depth=20)

        # A curse JC drain far larger than any shallow dig can earn.
        dig_service.set_temp_curse(10001, 12345, {
            "id": "test_drain", "name": "Draining Hex", "duration_digs": 3,
            "effect": {"jc_bonus": -9999},
        })
        balance_before = player_repository.get_balance(10001, 12345)

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        result = dig_service.dig(10001, 12345)
        assert result["success"]
        # Dig earnings floored at 0 — never negative from a curse.
        assert result["jc_earned"] == 0
        # The dig credited nothing; the balance did not go down from the curse.
        assert player_repository.get_balance(10001, 12345) == balance_before

    def test_curse_luminosity_drain_burns_extra_light(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A guttering-light curse drains extra luminosity on a dig beyond
        the layer's base drain."""
        _register_player(player_repository, 10001)
        _register_player(player_repository, 10002)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch, depth=20)
        _start_tunnel(dig_service, dig_repo, 10002, 12345, monkeypatch, depth=20)
        # Equal, known starting luminosity for a fair comparison.
        dig_repo.update_tunnel(10001, 12345, luminosity=100, last_lum_update_at=1_000_000)
        dig_repo.update_tunnel(10002, 12345, luminosity=100, last_lum_update_at=1_000_000)

        dig_service.set_temp_curse(10001, 12345, {
            "id": "test_gutter", "name": "Guttering Hex", "duration_digs": 3,
            "effect": {"luminosity_drain": 15},
        })

        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        monkeypatch.setattr(random, "random", lambda: 0.99)
        dig_service.dig(10001, 12345)
        dig_service.dig(10002, 12345)

        cursed = dig_repo.get_tunnel(10001, 12345)["luminosity"]
        clean = dig_repo.get_tunnel(10002, 12345)["luminosity"]
        # The curse drained an extra 15 light beyond the clean dig.
        assert cursed == clean - 15

    def test_curse_decrements_each_dig_and_expires(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """A 2-dig curse decrements once per dig and is gone after 2 digs."""
        _register_player(player_repository, 10001)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch, depth=20)

        dig_service.set_temp_curse(10001, 12345, {
            "id": "test_short", "name": "Brief Hex", "duration_digs": 2,
            "effect": {"advance_bonus": -1},
        })
        monkeypatch.setattr(random, "random", lambda: 0.99)

        # Dig 1: curse drops from 2 -> 1 remaining.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        dig_service.dig(10001, 12345)
        curse = dig_service._get_active_curse(dict(dig_repo.get_tunnel(10001, 12345)))
        assert curse is not None and curse["digs_remaining"] == 1

        # Dig 2: curse drops to 0 and is cleared.
        monkeypatch.setattr(
            time, "time", lambda: 1_000_000 + 2 * (FREE_DIG_COOLDOWN_SECONDS + 1),
        )
        dig_service.dig(10001, 12345)
        tunnel = dig_repo.get_tunnel(10001, 12345)
        assert dig_service._get_active_curse(dict(tunnel)) is None
        assert tunnel["temp_curses"] is None

    def test_expired_curse_no_longer_drains(
        self, dig_service, dig_repo, player_repository, monkeypatch,
    ):
        """Once a curse expires, a later dig advances as if it never lay —
        proving the effect actually stops, not just the counter."""
        _register_player(player_repository, 10001)
        _register_player(player_repository, 10002)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch, depth=20)
        _start_tunnel(dig_service, dig_repo, 10002, 12345, monkeypatch, depth=20)

        # Player 1 carries a 1-dig advance-draining curse.
        dig_service.set_temp_curse(10001, 12345, {
            "id": "test_oneshot", "name": "Fleeting Hex", "duration_digs": 1,
            "effect": {"advance_bonus": -1},
        })
        monkeypatch.setattr(random, "random", lambda: 0.99)
        monkeypatch.setattr(random, "randint", lambda a, b: b)

        # Dig 1 burns the curse for player 1.
        monkeypatch.setattr(time, "time", lambda: 1_000_000 + FREE_DIG_COOLDOWN_SECONDS + 1)
        dig_service.dig(10001, 12345)
        assert dig_service._get_active_curse(dict(dig_repo.get_tunnel(10001, 12345))) is None

        # Dig 2: re-park both at an identical depth so the only variable is
        # whether a curse is active — it no longer is, for either.
        dig_repo.update_tunnel(10001, 12345, depth=20)
        dig_repo.update_tunnel(10002, 12345, depth=20)
        monkeypatch.setattr(
            time, "time", lambda: 1_000_000 + 2 * (FREE_DIG_COOLDOWN_SECONDS + 1),
        )
        formerly_cursed = dig_service.dig(10001, 12345)
        clean = dig_service.dig(10002, 12345)
        assert formerly_cursed["advance"] == clean["advance"]


# ---------------------------------------------------------------------------
# JC threat — a real loss with no floor (can push into debt)
# ---------------------------------------------------------------------------


class TestJcThreat:
    """A failed risky pick on a bargain/theft event carries a real negative
    jc that is NOT floored — it can push the actor's balance below zero."""

    def test_jc_threat_failure_can_take_balance_negative(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """A large negative-jc failure outcome drives the balance below 0."""
        _register_player(player_repository, 10001, balance=30)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch)
        # Capture balance after the seed dig (which credits its own JC).
        balance_before = player_repository.get_balance(10001, 12345)
        assert balance_before >= 0

        event = _synthetic_event(
            "threat_jc_debt",
            risky_success=_outcome("The merchant pays fair.", jc=40),
            risky_failure=_outcome("The bottle empties your pockets.", jc=-200),
        )
        inject_event(event)
        # Force FAILURE; pin the jitter so the negative jc lands as authored.
        monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.99)
        monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)
        monkeypatch.setattr("services.dig.events_mixin.random.randint", lambda a, b: 0)

        result = dig_service.resolve_event(10001, 12345, event["id"], "risky")
        assert result["success"]
        assert result["jc_delta"] == -200
        # The balance went negative — into the debt system, no floor at 0.
        balance = player_repository.get_balance(10001, 12345)
        assert balance == balance_before - 200
        assert balance < 0
        # resolve_event surfaces the negative resulting balance for the embed.
        assert result.get("balance_after") == balance

    def test_jc_threat_success_pays_out_normally(
        self, dig_service, dig_repo, player_repository, monkeypatch, inject_event,
    ):
        """The same event's risky SUCCESS pays a positive jc — the no-floor
        rule only matters on the failure branch."""
        _register_player(player_repository, 10001, balance=30)
        _start_tunnel(dig_service, dig_repo, 10001, 12345, monkeypatch)
        balance_before = player_repository.get_balance(10001, 12345)

        event = _synthetic_event(
            "threat_jc_payout",
            risky_success=_outcome("The merchant pays fair.", jc=40),
            risky_failure=_outcome("Robbed blind.", jc=-200),
        )
        inject_event(event)
        monkeypatch.setattr("services.dig.events_mixin.random.random", lambda: 0.01)
        monkeypatch.setattr("services.dig.events_mixin.random.uniform", lambda a, b: 1.0)

        result = dig_service.resolve_event(10001, 12345, event["id"], "risky")
        assert result["success"]
        assert result["jc_delta"] == 40
        assert player_repository.get_balance(10001, 12345) == balance_before + 40
        # No debt surfaced on a positive outcome.
        assert result.get("balance_after") is None
