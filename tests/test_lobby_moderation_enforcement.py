"""Focused tests for lock-safe lobby suspension enforcement."""

from dataclasses import dataclass

import pytest

from domain.models.lobby import LobbyKind
from services.lobby_manager_service import (
    LobbyManagerService,
    LobbySuspendedError,
)
from services.lobby_service import LobbyService
from tests.fakes.lobby_repo import FakeLobbyRepo


@dataclass(frozen=True)
class FakeSuspension:
    discord_id: int
    guild_id: int
    scope: str


class FakeModerationService:
    def __init__(self):
        self.suspensions: dict[tuple[int, int, str], FakeSuspension] = {}
        self.calls: list[tuple[int, int, str | None]] = []

    def suspend(
        self,
        discord_id: int,
        guild_id: int,
        scope: str,
    ) -> FakeSuspension:
        suspension = FakeSuspension(discord_id, guild_id, scope)
        self.suspensions[(discord_id, guild_id, scope)] = suspension
        return suspension

    def get_active_suspension(
        self,
        discord_id: int,
        guild_id: int,
        lobby_kind: str | None = None,
    ) -> FakeSuspension | None:
        self.calls.append((discord_id, guild_id, lobby_kind))
        return self.suspensions.get(
            (discord_id, guild_id, "all")
        ) or self.suspensions.get((discord_id, guild_id, lobby_kind or "all"))


@pytest.mark.parametrize(
    ("scope", "blocked_kinds"),
    [
        ("all", {LobbyKind.OPEN, LobbyKind.LOWSKILL}),
        ("open", {LobbyKind.OPEN}),
        ("lowskill", {LobbyKind.LOWSKILL}),
    ],
)
def test_manager_join_enforces_all_and_kind_scoped_suspensions(
    scope,
    blocked_kinds,
):
    moderation = FakeModerationService()
    suspension = moderation.suspend(10, 99, scope)
    manager = LobbyManagerService(FakeLobbyRepo(), moderation_service=moderation)

    for kind in LobbyKind:
        result = manager.join_lobby(10, guild_id=99, lobby_kind=kind)

        if kind in blocked_kinds:
            assert result == "lobby_suspended"
            assert result.suspension is suspension
            lobby = manager.get_lobby(guild_id=99, lobby_kind=kind)
            assert lobby is None or 10 not in lobby.players
        else:
            assert result == "ok"
            assert manager.leave_lobby(10, guild_id=99, lobby_kind=kind)


def test_lobby_service_returns_exact_suspension_context_from_locked_join():
    moderation = FakeModerationService()
    suspension = moderation.suspend(10, 99, "open")
    manager = LobbyManagerService(FakeLobbyRepo())
    service = LobbyService(
        manager,
        player_repo=object(),
        moderation_service=moderation,
    )

    success, reason, context = service.join_lobby(
        10,
        guild_id=99,
        lobby_kind=LobbyKind.OPEN,
    )

    assert success is False
    assert reason == "lobby_suspended"
    assert context is suspension
    assert manager.get_lobby(guild_id=99, lobby_kind=LobbyKind.OPEN) is None


def test_suspended_creator_cannot_create_a_lobby():
    moderation = FakeModerationService()
    suspension = moderation.suspend(10, 99, "all")
    manager = LobbyManagerService(FakeLobbyRepo(), moderation_service=moderation)
    service = LobbyService(manager, player_repo=object())

    with pytest.raises(LobbySuspendedError) as exc_info:
        service.get_or_create_lobby(
            creator_id=10,
            guild_id=99,
            lobby_kind=LobbyKind.OPEN,
        )

    assert str(exc_info.value) == "lobby_suspended"
    assert exc_info.value.suspension is suspension
    assert manager.get_lobby(guild_id=99, lobby_kind=LobbyKind.OPEN) is None


def test_reservation_rechecks_suspensions_before_starting_future_match():
    moderation = FakeModerationService()
    manager = LobbyManagerService(FakeLobbyRepo(), moderation_service=moderation)
    assert manager.join_lobby(10, guild_id=99) == "ok"
    moderation.suspend(10, 99, "open")

    assert manager.reserve_lobby_players([10], guild_id=99) is False
    assert manager.get_in_flight_lobby_kind_for_player(10, guild_id=99) is None


def test_suspension_does_not_revoke_an_existing_reservation():
    moderation = FakeModerationService()
    manager = LobbyManagerService(FakeLobbyRepo(), moderation_service=moderation)
    assert manager.join_lobby(10, guild_id=99) == "ok"
    assert manager.reserve_lobby_players([10], guild_id=99) is True

    moderation.suspend(10, 99, "all")

    assert manager.reserve_lobby_players([10], guild_id=99) is False
    assert (
        manager.get_in_flight_lobby_kind_for_player(10, guild_id=99)
        is LobbyKind.OPEN
    )
    manager.release_lobby_players([10], guild_id=99)
    assert manager.get_in_flight_lobby_kind_for_player(10, guild_id=99) is None


def test_join_rolls_back_when_suspension_lands_during_the_join():
    """A suspension committed between the unlocked moderation read and the
    locked membership add must not leave the player seated: the post-add
    re-validation detects it and rolls the join back."""

    class RacingModerationService(FakeModerationService):
        def get_active_suspension(
            self,
            discord_id: int,
            guild_id: int,
            lobby_kind: str | None = None,
        ) -> FakeSuspension | None:
            result = super().get_active_suspension(
                discord_id,
                guild_id,
                lobby_kind,
            )
            if result is None and not self.suspensions:
                # The admin flow commits its suspension immediately after the
                # pre-join read returned nothing — classic TOCTOU sequencing.
                self.suspend(discord_id, guild_id, "all")
            return result

    moderation = RacingModerationService()
    repo = FakeLobbyRepo()
    manager = LobbyManagerService(repo, moderation_service=moderation)

    result = manager.join_lobby(10, guild_id=99)

    assert result == "lobby_suspended"
    assert result.suspension is moderation.suspensions[(10, 99, "all")]
    lobby = manager.get_lobby(guild_id=99, lobby_kind=LobbyKind.OPEN)
    assert lobby is not None and 10 not in lobby.players
    # The rollback is persisted, not just in-memory.
    assert all(10 not in row["players"] for row in repo.load_all_lobby_states())
    # Both moderation reads ran: the pre-join gate and the post-add re-check.
    assert len(moderation.calls) == 2


def test_join_rollback_leaves_reserved_player_seated():
    """A suspension landing mid-join must not evict a player an in-flight
    shuffle already reserved: the starting match proceeds with membership
    intact (same policy as leave_lobby and the admin suspend eviction)."""

    class RacingModerationService(FakeModerationService):
        manager: LobbyManagerService

        def get_active_suspension(
            self,
            discord_id: int,
            guild_id: int,
            lobby_kind: str | None = None,
        ) -> FakeSuspension | None:
            result = super().get_active_suspension(
                discord_id,
                guild_id,
                lobby_kind,
            )
            if result is None and not self.suspensions:
                # Between the pre-join gate and the post-add re-check, a
                # shuffle reserves the player and the admin commits a
                # suspension (injected directly: reserve_lobby_players would
                # recurse into this hook).
                self.manager._in_flight_player_kinds[(99, discord_id)] = (
                    LobbyKind.OPEN
                )
                self.suspend(discord_id, guild_id, "all")
            return result

    moderation = RacingModerationService()
    repo = FakeLobbyRepo()
    manager = LobbyManagerService(repo, moderation_service=moderation)
    moderation.manager = manager

    result = manager.join_lobby(10, guild_id=99)

    assert result == "ok"
    lobby = manager.get_lobby(guild_id=99, lobby_kind=LobbyKind.OPEN)
    assert lobby is not None and 10 in lobby.players
    assert any(10 in row["players"] for row in repo.load_all_lobby_states())


def test_moderation_queries_run_outside_manager_state_lock():
    """Suspension lookups are DB I/O (and can issue a lapsed-suspension close
    write), so they must never run inside the process-wide ``_state_lock``
    that the event loop also acquires synchronously (get_creation_lock)."""

    class LockCheckingModerationService(FakeModerationService):
        manager: LobbyManagerService

        def get_active_suspension(
            self,
            discord_id: int,
            guild_id: int,
            lobby_kind: str | None = None,
        ) -> FakeSuspension | None:
            assert not self.manager._state_lock._is_owned()
            return super().get_active_suspension(discord_id, guild_id, lobby_kind)

    moderation = LockCheckingModerationService()
    manager = LobbyManagerService(FakeLobbyRepo(), moderation_service=moderation)
    moderation.manager = manager

    assert manager.join_lobby(10, guild_id=99) == "ok"
    assert manager.reserve_lobby_players([10], guild_id=99) is True
    manager.release_lobby_players([10], guild_id=99)
    manager.reset_lobby(guild_id=99)
    moderation.suspend(20, 99, "open")

    with pytest.raises(LobbySuspendedError):
        manager.get_or_create_lobby(creator_id=20, guild_id=99)

    # The unlocked lookup must have actually been exercised on every path.
    assert moderation.calls
