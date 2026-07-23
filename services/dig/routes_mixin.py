"""Persistent route choices between Dig boss tiers."""

from __future__ import annotations

import json
import random

from repositories.dig_repository import TunnelStateConflictError
from services.dig_constants import BOSS_BOUNDARIES, LAYERS, PINNACLE_DEPTH
from services.dig_data.routes import ROUTE_BY_ID, DigRoute, generate_route_offer


class RoutesMixin:
    """Own route offers, selection, and active route modifiers."""

    @staticmethod
    def _route_to_dict(route: DigRoute) -> dict:
        return {
            "id": route.id,
            "name": route.name,
            "description": route.description,
            "layer": route.layer,
            "effects": dict(route.effects),
        }

    @staticmethod
    def _parse_route_state(tunnel: dict | None) -> dict | None:
        if not tunnel:
            return None
        raw = tunnel.get("route_state")
        if not raw:
            return None
        try:
            state = json.loads(raw) if isinstance(raw, str) else dict(raw)
        except (TypeError, ValueError, json.JSONDecodeError):
            return None
        offered = state.get("offered")
        selected = state.get("selected")
        if (
            not isinstance(state.get("layer"), str)
            or not isinstance(offered, list)
            or len(offered) != 3
            or any(not isinstance(route_id, str) or route_id not in ROUTE_BY_ID for route_id in offered)
            or selected is not None and selected not in offered
        ):
            return None
        return state

    @staticmethod
    def _serialize_route_state(state: dict) -> str:
        return json.dumps(state, sort_keys=True)

    def _build_route_offer_state(
        self,
        tunnel: dict,
        defeated_boundary: int,
        *,
        rng=random,
    ) -> dict | None:
        """Build the next layer's persisted three-route offer."""
        if defeated_boundary not in BOSS_BOUNDARIES:
            return None
        boundary_index = BOSS_BOUNDARIES.index(defeated_boundary)
        next_layer = LAYERS[boundary_index + 1]["name"]
        end_depth = (
            BOSS_BOUNDARIES[boundary_index + 1]
            if boundary_index + 1 < len(BOSS_BOUNDARIES)
            else PINNACLE_DEPTH
        )
        previous = self._parse_route_state(tunnel)
        previous_route_id = next(
            (
                route_id
                for route_id in previous["offered"]
                if ROUTE_BY_ID[route_id].layer is None
            ),
            None,
        ) if previous else None
        offered = generate_route_offer(
            next_layer,
            previous_route_id=previous_route_id,
            rng=rng,
        )
        return {
            "layer": next_layer,
            "start_depth": defeated_boundary,
            "end_depth": end_depth,
            "offered": list(offered),
            "selected": None,
        }

    def _get_active_route(self, tunnel: dict | None) -> DigRoute | None:
        state = self._parse_route_state(tunnel)
        if state is None or state.get("selected") is None:
            return None
        return ROUTE_BY_ID.get(state["selected"])

    def _get_route_effects(self, tunnel: dict | None) -> dict:
        route = self._get_active_route(tunnel)
        return dict(route.effects) if route is not None else {}

    @staticmethod
    def _route_luminosity_drain_factor(route_effects: dict) -> float:
        return max(
            0.0,
            1.0
            + float(route_effects.get("luminosity_drain_multiplier", 0))
            - float(route_effects.get("luminosity_drain_reduction", 0)),
        )

    @staticmethod
    def _apply_route_cave_in_loss(
        block_loss: int,
        route_effects: dict,
        *additional_caps: int | None,
    ) -> int:
        block_loss += int(route_effects.get("cave_in_loss_bonus", 0))
        loss_cap = RoutesMixin._effective_cave_in_loss_cap(
            route_effects,
            *additional_caps,
        )
        if loss_cap is not None:
            block_loss = min(block_loss, loss_cap)
        return max(0, block_loss)

    @staticmethod
    def _effective_cave_in_loss_cap(
        route_effects: dict,
        *additional_caps: int | None,
    ) -> int | None:
        active_caps = [
            int(cap)
            for cap in (route_effects.get("cave_in_loss_cap"), *additional_caps)
            if cap is not None
        ]
        return min(active_caps) if active_caps else None

    def _route_status_from_tunnel(self, tunnel: dict) -> dict:
        state = self._parse_route_state(tunnel)
        if state is None:
            return {
                "choice_required": False,
                "layer": None,
                "offered_routes": [],
                "active_route": None,
            }
        selected = state.get("selected")
        return {
            "choice_required": selected is None,
            "layer": state["layer"],
            "start_depth": state.get("start_depth"),
            "end_depth": state.get("end_depth"),
            "offered_routes": [
                self._route_to_dict(ROUTE_BY_ID[route_id])
                for route_id in state["offered"]
            ],
            "active_route": (
                self._route_to_dict(ROUTE_BY_ID[selected]) if selected else None
            ),
        }

    def _pending_route_result(self, tunnel: dict) -> dict | None:
        status = self._route_status_from_tunnel(tunnel)
        if not status["choice_required"]:
            return None
        return {
            "success": False,
            "error": "Choose a route before continuing your expedition.",
            "route_choice_required": True,
            **status,
        }

    def get_route_status(self, discord_id: int, guild_id) -> dict:
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")
        return self._ok(**self._route_status_from_tunnel(dict(tunnel)))

    def choose_route(self, discord_id: int, guild_id, route_id: str) -> dict:
        tunnel = self.dig_repo.get_tunnel(discord_id, guild_id)
        if tunnel is None:
            return self._error("You don't have a tunnel.")
        tunnel = dict(tunnel)
        state = self._parse_route_state(tunnel)
        if state is None:
            return self._error("There is no route choice waiting for you.")

        selected = state.get("selected")
        if selected is not None:
            if selected == route_id:
                return self._ok(
                    route=self._route_to_dict(ROUTE_BY_ID[selected]),
                    already_selected=True,
                )
            return self._error("Your route for this layer is already locked in.")
        if route_id not in state["offered"]:
            return self._error("That route was not offered for this layer.")

        raw_pending = tunnel["route_state"]
        selected_state = {**state, "selected": route_id}
        selected_json = self._serialize_route_state(selected_state)
        try:
            self.dig_repo.atomic_tunnel_balance_update(
                discord_id,
                guild_id,
                tunnel_updates={"route_state": selected_json},
                require_tunnel_state={"route_state": raw_pending},
                log_detail={"route_id": route_id, "layer": state["layer"]},
                log_action_type="route_choice",
            )
        except TunnelStateConflictError:
            current = self.dig_repo.get_tunnel(discord_id, guild_id)
            current_state = self._parse_route_state(dict(current)) if current else None
            if current_state and current_state.get("selected") == route_id:
                return self._ok(
                    route=self._route_to_dict(ROUTE_BY_ID[route_id]),
                    already_selected=True,
                )
            return self._error("Your route choice changed before it could be saved.")

        return self._ok(
            route=self._route_to_dict(ROUTE_BY_ID[route_id]),
            already_selected=False,
        )
