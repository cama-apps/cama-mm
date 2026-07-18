"""Typed public effects for the active server-wide economy event."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any


@dataclass(frozen=True)
class EconomyEventEffects:
    """Neutral defaults make event integration safe and backwards-compatible."""

    reward_multiplier: float = 1.0
    gamba_win_multiplier: float = 1.0
    gamba_loss_multiplier: float = 1.0
    bet_payout_multiplier: float = 1.0
    prediction_payout_multiplier: float = 1.0
    prediction_depth_multiplier: float = 1.0
    prediction_spread_ticks_delta: int = 0
    reserve_burn_jc: int = 0
    reserve_release_jc: int = 0
    wallet_burn_rate: float = 0.0
    wallet_burn_jc: int = 0

    @classmethod
    def from_mapping(cls, raw: dict[str, Any] | None) -> EconomyEventEffects:
        raw = raw or {}
        return cls(
            reward_multiplier=float(raw.get("reward_multiplier", 1.0)),
            gamba_win_multiplier=float(raw.get("gamba_win_multiplier", 1.0)),
            gamba_loss_multiplier=float(raw.get("gamba_loss_multiplier", 1.0)),
            bet_payout_multiplier=float(raw.get("bet_payout_multiplier", 1.0)),
            prediction_payout_multiplier=float(
                raw.get("prediction_payout_multiplier", 1.0)
            ),
            prediction_depth_multiplier=float(
                raw.get("prediction_depth_multiplier", 1.0)
            ),
            prediction_spread_ticks_delta=int(
                raw.get("prediction_spread_ticks_delta", 0)
            ),
            reserve_burn_jc=int(raw.get("reserve_burn_jc", 0)),
            reserve_release_jc=int(raw.get("reserve_release_jc", 0)),
            wallet_burn_rate=float(raw.get("wallet_burn_rate", 0.0)),
            wallet_burn_jc=int(raw.get("wallet_burn_jc", 0)),
        )

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


NEUTRAL_ECONOMY_EFFECTS = EconomyEventEffects()
