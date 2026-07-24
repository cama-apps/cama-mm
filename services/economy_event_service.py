"""Daily Dota-themed monetary events and the Jopacoin feedback controller."""

from __future__ import annotations

import datetime as dt
import hashlib
import math
import random
import time
from dataclasses import dataclass
from typing import Any
from zoneinfo import ZoneInfo

from config import (
    ECONOMY_EVENT_LOOKBACK_DAYS,
    ECONOMY_EVENT_MAX_RESERVE_BURN_PCT,
    ECONOMY_EVENT_MAX_WALLET_BURN_PCT,
    ECONOMY_EVENT_TRIGGER_HOUR_LOCAL,
    ECONOMY_EVENTS_ENABLED,
    ECONOMY_INFLATION_CEILING,
    ECONOMY_NORMAL_ANNUAL_RATE,
    ECONOMY_RECOVERY_ANNUAL_RATE,
    ECONOMY_RECOVERY_MODE,
)
from domain.models.economy_event import (
    NEUTRAL_ECONOMY_EFFECTS,
    EconomyEventEffects,
)
from repositories.economy_event_repository import EconomyEventRepository

_EVENT_TIMEZONE = ZoneInfo("America/Los_Angeles")


@dataclass(frozen=True)
class _EventTemplate:
    name: str
    hero: str
    direction: str
    flavor: str
    reward_step: float = 0.0
    gamba_win_step: float = 0.0
    gamba_loss_step: float = 0.0
    bet_step: float = 0.0
    prediction_payout_step: float = 0.0
    spread_step: int = 0
    reserve_burn_step: float = 0.0
    reserve_release_step: float = 0.0
    wallet_burn_step: float = 0.0


_EVENT_CATALOG = (
    _EventTemplate(
        "Ravage", "Tidehunter", "deflationary",
        "A tidal shock tears through every corner of the Jopacoin economy.",
        reward_step=-0.08, gamba_win_step=-0.03, gamba_loss_step=0.04,
        bet_step=-0.015, prediction_payout_step=-0.005,
        spread_step=1, reserve_burn_step=0.004,
    ),
    _EventTemplate(
        "Black Hole", "Enigma", "deflationary",
        "Liquidity is dragged toward a singularity and refuses to escape.",
        reward_step=-0.05, gamba_win_step=-0.04, gamba_loss_step=0.08,
        bet_step=-0.02, prediction_payout_step=-0.008,
        spread_step=2, reserve_burn_step=0.003,
    ),
    _EventTemplate(
        "Doom", "Doom", "deflationary",
        "The server's opt-in rewards have been marked for destruction.",
        reward_step=-0.15, gamba_win_step=-0.08, gamba_loss_step=0.10,
        bet_step=-0.025, prediction_payout_step=-0.01,
        spread_step=1,
    ),
    _EventTemplate(
        "Echo Slam", "Earthshaker", "deflationary",
        "Every transaction makes the next shock hit harder.",
        reward_step=-0.07, gamba_win_step=-0.04, gamba_loss_step=0.12,
        bet_step=-0.015, prediction_payout_step=-0.005,
        spread_step=1, reserve_burn_step=0.002,
    ),
    _EventTemplate(
        "Reaper's Scythe", "Necrophos", "deflationary",
        "Large voluntary risks now carry a much sharper edge.",
        reward_step=-0.04, gamba_win_step=-0.10, gamba_loss_step=0.14,
        bet_step=-0.035, prediction_payout_step=-0.012,
        spread_step=1,
    ),
    _EventTemplate(
        "Global Silence", "Silencer", "deflationary",
        "Bonus rewards vanish and market makers fall quiet.",
        reward_step=-0.12, gamba_win_step=-0.03, bet_step=-0.01,
        spread_step=2,
    ),
    _EventTemplate(
        "Sanity's Eclipse", "Outworld Destroyer", "deflationary",
        "A rare hard shock erases a thin layer of exposed liquidity.",
        reward_step=-0.05, gamba_win_step=-0.03, bet_step=-0.01,
        prediction_payout_step=-0.005, spread_step=1,
        wallet_burn_step=0.0005,
    ),
    _EventTemplate(
        "Chronosphere", "Faceless Void", "neutral",
        "Time stops around the order books while the wider economy catches up.",
        spread_step=2,
    ),
    _EventTemplate(
        "Song of the Siren", "Naga Siren", "neutral",
        "Volatility sleeps; both upside and downside soften for a day.",
        gamba_win_step=-0.04, gamba_loss_step=-0.04,
        spread_step=1,
    ),
    _EventTemplate(
        "Sunder", "Terrorblade", "neutral",
        "Wallet and Reserve liquidity trade places without changing supply.",
        reserve_release_step=0.002,
    ),
    _EventTemplate(
        "Hand of God", "Chen", "boon",
        "The Reserve opens and every voluntary economy surface receives aid.",
        reward_step=0.05, gamba_win_step=0.05, gamba_loss_step=-0.04,
        bet_step=0.015, prediction_payout_step=0.005,
        spread_step=-1, reserve_release_step=0.004,
    ),
    _EventTemplate(
        "Guardian Angel", "Omniknight", "boon",
        "Losses are softened and market liquidity receives divine protection.",
        reward_step=0.03, gamba_win_step=0.03, gamba_loss_step=-0.08,
        bet_step=0.01, prediction_payout_step=0.004,
        spread_step=-1,
    ),
    _EventTemplate(
        "Reincarnation", "Wraith King", "boon",
        "Defeated wagers rise again with part of their value restored.",
        reward_step=0.04, gamba_win_step=0.06, gamba_loss_step=-0.10,
        bet_step=0.015, prediction_payout_step=0.006,
        spread_step=-1,
    ),
    _EventTemplate(
        "Stampede", "Centaur Warrunner", "boon",
        "Activity surges and liquidity charges into every open market.",
        reward_step=0.10, gamba_win_step=0.04, gamba_loss_step=-0.03,
        bet_step=0.02, prediction_payout_step=0.005,
        spread_step=-1,
    ),
    _EventTemplate(
        "Supernova", "Phoenix", "boon",
        "Reserve liquidity is reborn as a burst of server-wide economic heat.",
        reward_step=0.08, gamba_win_step=0.07, gamba_loss_step=-0.05,
        bet_step=0.02, prediction_payout_step=0.008,
        spread_step=-1, reserve_release_step=0.006,
    ),
)


class EconomyEventService:
    """Select, apply, expose, and report one monetary event per guild-day."""

    def __init__(
        self,
        repository: EconomyEventRepository,
        *,
        enabled: bool = ECONOMY_EVENTS_ENABLED,
        recovery_mode: bool = ECONOMY_RECOVERY_MODE,
        recovery_annual_rate: float = ECONOMY_RECOVERY_ANNUAL_RATE,
        normal_annual_rate: float = ECONOMY_NORMAL_ANNUAL_RATE,
        inflation_ceiling: float = ECONOMY_INFLATION_CEILING,
        lookback_days: int = ECONOMY_EVENT_LOOKBACK_DAYS,
        max_reserve_burn_pct: float = ECONOMY_EVENT_MAX_RESERVE_BURN_PCT,
        max_wallet_burn_pct: float = ECONOMY_EVENT_MAX_WALLET_BURN_PCT,
        trigger_hour_local: int = ECONOMY_EVENT_TRIGGER_HOUR_LOCAL,
    ):
        self.repository = repository
        self.enabled = bool(enabled)
        self.recovery_mode = bool(recovery_mode)
        self.recovery_annual_rate = min(-0.0001, float(recovery_annual_rate))
        self.normal_annual_rate = min(
            float(inflation_ceiling), max(-0.99, float(normal_annual_rate))
        )
        self.inflation_ceiling = float(inflation_ceiling)
        self.lookback_days = max(1, int(lookback_days))
        self.max_reserve_burn_pct = min(1.0, max(0.0, float(max_reserve_burn_pct)))
        self.max_wallet_burn_pct = min(0.05, max(0.0, float(max_wallet_burn_pct)))
        self.trigger_hour_local = min(23, max(0, int(trigger_hour_local)))

    def _event_date_for_timestamp(self, timestamp: int | float) -> str:
        """Return the Pacific-local event date active at ``timestamp``."""
        local = dt.datetime.fromtimestamp(timestamp, tz=dt.UTC).astimezone(
            _EVENT_TIMEZONE
        )
        event_day = local.date()
        if local.hour < self.trigger_hour_local:
            event_day -= dt.timedelta(days=1)
        return event_day.isoformat()

    def _event_window(self, event_date: str) -> tuple[int, int]:
        """Return DST-aware Pacific trigger boundaries for an event date."""
        event_day = dt.date.fromisoformat(event_date)
        starts_local = dt.datetime.combine(
            event_day,
            dt.time(hour=self.trigger_hour_local),
            tzinfo=_EVENT_TIMEZONE,
        )
        ends_local = dt.datetime.combine(
            event_day + dt.timedelta(days=1),
            dt.time(hour=self.trigger_hour_local),
            tzinfo=_EVENT_TIMEZONE,
        )
        return int(starts_local.timestamp()), int(ends_local.timestamp())

    def seconds_until_next_trigger(self, now: int | float | None = None) -> int:
        """Return whole seconds until the next Pacific-local event trigger."""
        now = float(now if now is not None else time.time())
        local_now = dt.datetime.fromtimestamp(now, tz=dt.UTC).astimezone(
            _EVENT_TIMEZONE
        )
        next_local = dt.datetime.combine(
            local_now.date(),
            dt.time(hour=self.trigger_hour_local),
            tzinfo=_EVENT_TIMEZONE,
        )
        if local_now >= next_local:
            next_local = dt.datetime.combine(
                local_now.date() + dt.timedelta(days=1),
                dt.time(hour=self.trigger_hour_local),
                tzinfo=_EVENT_TIMEZONE,
            )
        return max(0, int(math.ceil(next_local.timestamp() - now)))

    def ensure_policy(self, guild_id: int | None, *, now: int | None = None) -> dict:
        mode = "recovery" if self.recovery_mode else "normal"
        target = (
            self.recovery_annual_rate if mode == "recovery" else self.normal_annual_rate
        )
        return self.repository.ensure_policy_state(
            guild_id,
            mode=mode if self.enabled else "disabled",
            target_annual_rate=target,
            inflation_ceiling=self.inflation_ceiling,
            now=now,
        )

    def get_effects(self, guild_id: int | None) -> EconomyEventEffects:
        if not self.enabled:
            return NEUTRAL_ECONOMY_EFFECTS
        event_date = self._event_date_for_timestamp(time.time())
        event = self.repository.get_event_for_date(guild_id, event_date)
        if not event:
            return NEUTRAL_ECONOMY_EFFECTS
        return EconomyEventEffects.from_mapping(event.get("effects"))

    def adjust_reward(self, guild_id: int | None, amount: int) -> int:
        """Apply the active event to a positive opt-in reward."""
        if amount <= 0:
            return amount
        multiplier = self.get_effects(guild_id).reward_multiplier
        return max(0, int(float(amount) * multiplier + 0.5))

    @staticmethod
    def _severity_for_correction(required_effect: int, deadband: int) -> int:
        """Map the policy-correction magnitude to a stable event level."""
        unit = max(1, int(deadband))
        magnitude = abs(int(required_effect))
        if magnitude <= unit * 5:
            return 1
        if magnitude <= unit * 15:
            return 2
        return 3

    def ensure_daily_event(
        self,
        guild_id: int | None,
        *,
        now: int | None = None,
        event_date: str | None = None,
    ) -> tuple[dict[str, Any] | None, bool]:
        """Create/apply today's 10 AM card once; retries return the active card."""
        now = int(now if now is not None else time.time())
        explicit_event_date = event_date is not None
        event_date = event_date or self._event_date_for_timestamp(now)
        policy = self.ensure_policy(guild_id, now=now)
        if not self.enabled or policy.get("mode") == "disabled":
            return None, False

        existing = self.repository.get_event_for_date(guild_id, event_date)
        if existing:
            return existing, False

        # Before today's trigger, yesterday is still the active event date. A
        # missing card at that point must remain neutral instead of applying a
        # late direct burn for most of an already elapsed window. Explicit
        # event dates intentionally bypass this scheduler guard.
        local_now = dt.datetime.fromtimestamp(now, tz=dt.UTC).astimezone(
            _EVENT_TIMEZONE
        )
        if not explicit_event_date and local_now.hour < self.trigger_hour_local:
            return None, False

        before = self.repository.capture_balance_sheet(guild_id)
        monetary_stock = int(before["monetary_stock"])
        self.repository.reconcile_prior_event(guild_id, event_date, monetary_stock)
        forecast = self.repository.forecast_daily_flow(
            guild_id, lookback_days=self.lookback_days, now=now
        )
        target_rate = float(policy["target_annual_rate"])
        target_daily_change = int(
            round(monetary_stock * (math.pow(1.0 + target_rate, 1.0 / 365.0) - 1.0))
        )
        required_effect = target_daily_change - forecast
        deadband = max(5, int(round(abs(monetary_stock) * 0.0001)))
        if required_effect < -deadband:
            direction = "deflationary"
        elif required_effect > deadband:
            direction = "boon"
        else:
            direction = "neutral"
        severity = self._severity_for_correction(required_effect, deadband)

        volumes = self.repository.get_surface_daily_volumes(
            guild_id, lookback_days=self.lookback_days, now=now
        )
        candidates: list[tuple[int, _EventTemplate, int, dict[str, Any], int]] = []
        for template in _EVENT_CATALOG:
            if template.direction != direction:
                continue
            effects = self._effects_for(template, severity, before)
            expected = self._estimate_effect(effects, volumes, before)
            distance = abs(required_effect - expected)
            candidates.append((distance, template, severity, effects, expected))
        candidates.sort(key=lambda item: item[0])
        shortlist = candidates[: min(3, len(candidates))]
        seed_bytes = hashlib.sha256(
            f"{self.repository.normalize_guild_id(guild_id)}:{event_date}".encode()
        ).digest()
        rng = random.Random(int.from_bytes(seed_bytes[:8], "big"))
        _, template, severity, effects, expected = rng.choice(shortlist)

        starts_at, ends_at = self._event_window(event_date)
        announcement = self._announcement_text(
            template, severity, effects, required_effect, forecast
        )
        event, created = self.repository.activate_event_atomic(
            guild_id,
            {
                "event_date": event_date,
                "name": template.name,
                "hero": template.hero,
                "direction": template.direction,
                "severity": severity,
                "target_effect_jc": required_effect,
                "forecast_flow_jc": forecast,
                "expected_effect_jc": expected,
                "monetary_stock_before": monetary_stock,
                "effects": effects,
                "announcement": announcement,
                "starts_at": starts_at,
                "ends_at": ends_at,
                "created_at": now,
            },
        )
        after = self.repository.capture_balance_sheet(guild_id)
        self.repository.save_snapshot(
            guild_id, event_date, after, captured_at=now
        )
        return event, created

    def mark_event_announced(
        self, guild_id: int | None, event_id: int, *, now: int | None = None
    ) -> None:
        """Record that an event's public announcement was delivered."""
        self.repository.mark_event_announced(guild_id, event_id, now=now)

    def get_policy_status(self, guild_id: int | None) -> dict[str, Any]:
        policy = self.ensure_policy(guild_id)
        snapshot = self.repository.capture_balance_sheet(guild_id)
        latest = self.repository.get_latest_snapshot(guild_id)
        event_date = self._event_date_for_timestamp(time.time())
        event = self.repository.get_event_for_date(guild_id, event_date)
        return {
            "policy": policy,
            "balance_sheet": snapshot,
            "latest_snapshot": latest,
            "event": event,
            "effects": EconomyEventEffects.from_mapping(
                event.get("effects") if event else None
            ),
        }

    def format_event(self, event: dict[str, Any]) -> tuple[str, str]:
        level = ("I", "II", "III")[max(1, min(3, int(event["severity"]))) - 1]
        effects = EconomyEventEffects.from_mapping(event.get("effects"))
        lines = [event.get("announcement") or "The economy has shifted."]
        direct = []
        if effects.reserve_burn_jc:
            direct.append(f"Reserve burned: **{effects.reserve_burn_jc:,} JC**")
        if effects.wallet_burn_jc:
            direct.append(f"Wallet liquidity burned: **{effects.wallet_burn_jc:,} JC**")
        if effects.reserve_release_jc:
            direct.append(f"Reserve released: **{effects.reserve_release_jc:,} JC**")
        if direct:
            lines.append("\n".join(direct))
        return f"{event['name']} — Level {level}", "\n\n".join(lines)

    def _effects_for(
        self,
        template: _EventTemplate,
        severity: int,
        balance_sheet: dict[str, int | float],
    ) -> dict[str, Any]:
        def multiplier(step: float, *, low: float = 0.25, high: float = 2.0) -> float:
            return round(min(high, max(low, 1.0 + step * severity)), 4)

        available = max(0, int(balance_sheet["reserve_available"]))
        burn_pct = min(
            self.max_reserve_burn_pct,
            max(0.0, template.reserve_burn_step * severity),
        )
        wallet_burn_rate = min(
            self.max_wallet_burn_pct,
            max(0.0, template.wallet_burn_step * severity),
        )
        release_pct = max(0.0, template.reserve_release_step * severity)
        return {
            "reward_multiplier": multiplier(template.reward_step),
            "gamba_win_multiplier": multiplier(template.gamba_win_step),
            "gamba_loss_multiplier": multiplier(template.gamba_loss_step),
            "bet_payout_multiplier": multiplier(template.bet_step, low=0.5, high=1.5),
            "prediction_payout_multiplier": multiplier(
                template.prediction_payout_step, low=0.9, high=1.1
            ),
            "prediction_depth_multiplier": 1.0,
            "prediction_spread_ticks_delta": template.spread_step * severity,
            "reserve_burn_jc": int(available * burn_pct),
            "reserve_release_jc": int(available * release_pct),
            "wallet_burn_rate": round(wallet_burn_rate, 6),
        }

    @staticmethod
    def _estimate_effect(
        effects: dict[str, Any],
        volumes: dict[str, float],
        balance_sheet: dict[str, int | float],
    ) -> int:
        expected = 0.0
        expected += volumes["reward_credits"] * (effects["reward_multiplier"] - 1.0)
        expected += volumes["gamba_credits"] * (
            effects["gamba_win_multiplier"] - 1.0
        )
        # Much of a gamba loss moves into the Reserve instead of burning; use a
        # conservative sink yield until observed event data can replace it.
        expected -= 0.25 * volumes["gamba_debits"] * (
            effects["gamba_loss_multiplier"] - 1.0
        )
        expected += volumes["bet_payouts"] * (
            effects["bet_payout_multiplier"] - 1.0
        )
        expected += volumes["prediction_payouts"] * (
            effects["prediction_payout_multiplier"] - 1.0
        )
        expected -= int(effects.get("reserve_burn_jc", 0))
        # Reserve releases move existing JC into wallets. Both accounts are
        # already included in monetary_stock, so the net supply effect is zero.
        expected -= int(
            float(balance_sheet["positive_wallets"])
            * float(effects.get("wallet_burn_rate", 0.0))
        )
        return int(round(expected))

    @staticmethod
    def _announcement_text(
        template: _EventTemplate,
        severity: int,
        effects: dict[str, Any],
        required_effect: int,
        forecast: int,
    ) -> str:
        lines = [template.flavor]
        if effects["reserve_burn_jc"]:
            lines.append(
                f"The uncommitted Jopa Reserve loses **{effects['reserve_burn_jc']:,} JC**."
            )
        if effects["reserve_release_jc"]:
            lines.append(
                f"The Jopa Reserve releases **{effects['reserve_release_jc']:,} JC**."
            )
        if effects["wallet_burn_rate"]:
            lines.append(
                f"Positive wallets lose **{effects['wallet_burn_rate'] * 100:.2f}%**."
            )
        if effects["reward_multiplier"] != 1.0:
            change = (effects["reward_multiplier"] - 1.0) * 100
            lines.append(
                "Generated rewards (dig, trivia, and mana): "
                f"**{change:+.0f}%**."
            )
        if (
            effects["gamba_win_multiplier"] != 1.0
            or effects["gamba_loss_multiplier"] != 1.0
        ):
            win = (effects["gamba_win_multiplier"] - 1.0) * 100
            loss = (effects["gamba_loss_multiplier"] - 1.0) * 100
            lines.append(f"Gamba wins: **{win:+.0f}%**; losses: **{loss:+.0f}%**.")
        if effects["bet_payout_multiplier"] != 1.0:
            change = (effects["bet_payout_multiplier"] - 1.0) * 100
            lines.append(f"Placed-bet payouts resolving today: **{change:+.1f}%**.")
        pred_payout = (effects["prediction_payout_multiplier"] - 1.0) * 100
        spread = int(effects["prediction_spread_ticks_delta"])
        prediction_parts = []
        if pred_payout:
            prediction_parts.append(f"resolution **{pred_payout:+.1f}%**")
        if spread:
            prediction_parts.append(f"spread **{spread:+d} ticks**")
        if prediction_parts:
            lines.append(f"Prediction markets: {', '.join(prediction_parts)}.")
        lines.append(
            f"Policy target: **{required_effect:+,} JC** after a "
            f"**{forecast:+,} JC/day** unmanaged-flow forecast."
        )
        if severity == 3:
            lines.append("**Aghanim's Scepter intensity is active.**")
        return "\n".join(lines)
