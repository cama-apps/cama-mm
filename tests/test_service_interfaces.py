"""Tests for service interfaces."""

import inspect
from abc import ABC

import pytest


class TestServiceInterfacesExist:
    """Test that all expected interfaces are defined."""

    def test_interfaces_module_imports(self):
        """Service interfaces module can be imported."""
        from services import interfaces

        assert interfaces is not None

    def test_player_service_interface(self):
        """IPlayerService interface exists with expected methods."""
        from services.interfaces import IPlayerService

        assert issubclass(IPlayerService, ABC)
        assert hasattr(IPlayerService, "register_player")
        assert hasattr(IPlayerService, "get_player")
        assert hasattr(IPlayerService, "set_roles")
        assert hasattr(IPlayerService, "get_balance")
        assert hasattr(IPlayerService, "get_stats")

    def test_match_service_interface(self):
        """IMatchService interface exists with expected methods."""
        from services.interfaces import IMatchService

        assert issubclass(IMatchService, ABC)
        assert hasattr(IMatchService, "shuffle_players")
        assert hasattr(IMatchService, "record_match")
        assert hasattr(IMatchService, "get_last_shuffle")
        assert hasattr(IMatchService, "add_record_submission")
        assert hasattr(IMatchService, "can_record_match")

    def test_betting_service_interface(self):
        """IBettingService interface exists with expected methods."""
        from services.interfaces import IBettingService

        assert issubclass(IBettingService, ABC)
        assert hasattr(IBettingService, "place_bet")
        assert hasattr(IBettingService, "settle_bets")
        assert hasattr(IBettingService, "get_pot_odds")
        assert hasattr(IBettingService, "refund_pending_bets")

    def test_loan_service_interface(self):
        """ILoanService interface exists with expected methods."""
        from services.interfaces import ILoanService

        assert issubclass(ILoanService, ABC)
        assert hasattr(ILoanService, "get_state")
        assert hasattr(ILoanService, "validate_loan")
        assert hasattr(ILoanService, "execute_loan")
        assert hasattr(ILoanService, "execute_repayment")

    def test_bankruptcy_service_interface(self):
        """IBankruptcyService interface exists with expected methods."""
        from services.interfaces import IBankruptcyService

        assert issubclass(IBankruptcyService, ABC)
        assert hasattr(IBankruptcyService, "get_state")
        assert hasattr(IBankruptcyService, "validate_bankruptcy")
        assert hasattr(IBankruptcyService, "execute_bankruptcy")
        assert hasattr(IBankruptcyService, "apply_penalty_to_winnings")

    def test_prediction_service_interface(self):
        """IPredictionService interface exists with expected methods."""
        from services.interfaces import IPredictionService

        assert issubclass(IPredictionService, ABC)
        assert hasattr(IPredictionService, "create_prediction")
        assert hasattr(IPredictionService, "place_bet")
        assert hasattr(IPredictionService, "resolve")
        assert hasattr(IPredictionService, "cancel")

    def test_lobby_service_interface(self):
        """ILobbyService interface exists with expected methods."""
        from services.interfaces import ILobbyService

        assert issubclass(ILobbyService, ABC)
        assert hasattr(ILobbyService, "get_or_create_lobby")
        assert hasattr(ILobbyService, "join_lobby")
        assert hasattr(ILobbyService, "leave_lobby")
        assert hasattr(ILobbyService, "reset_lobby")

    def test_gambling_stats_service_interface(self):
        """IGamblingStatsService interface exists with expected methods."""
        from services.interfaces import IGamblingStatsService

        assert issubclass(IGamblingStatsService, ABC)
        assert hasattr(IGamblingStatsService, "get_player_stats")
        assert hasattr(IGamblingStatsService, "calculate_degen_score")
        assert hasattr(IGamblingStatsService, "get_leaderboard")


class TestInterfaceMethodsAreAbstract:
    """Test that interface methods are properly abstract."""

    def test_player_service_methods_abstract(self):
        """IPlayerService methods are abstract."""
        from services.interfaces import IPlayerService

        # Get all methods defined in the interface
        methods = inspect.getmembers(IPlayerService, predicate=inspect.isfunction)
        for name, method in methods:
            if not name.startswith("_"):
                # Check that it's abstract
                assert getattr(method, "__isabstractmethod__", False), (
                    f"Method {name} should be abstract"
                )

    def test_match_service_methods_abstract(self):
        """IMatchService methods are abstract."""
        from services.interfaces import IMatchService

        methods = inspect.getmembers(IMatchService, predicate=inspect.isfunction)
        for name, method in methods:
            if not name.startswith("_"):
                assert getattr(method, "__isabstractmethod__", False), (
                    f"Method {name} should be abstract"
                )


class TestInterfaceCantBeInstantiated:
    """Test that interfaces cannot be directly instantiated."""

    def test_player_service_not_instantiable(self):
        """IPlayerService cannot be instantiated."""
        from services.interfaces import IPlayerService

        with pytest.raises(TypeError):
            IPlayerService()  # type: ignore

    def test_match_service_not_instantiable(self):
        """IMatchService cannot be instantiated."""
        from services.interfaces import IMatchService

        with pytest.raises(TypeError):
            IMatchService()  # type: ignore

    def test_betting_service_not_instantiable(self):
        """IBettingService cannot be instantiated."""
        from services.interfaces import IBettingService

        with pytest.raises(TypeError):
            IBettingService()  # type: ignore


class TestAllInterfacesPresent:
    """Ensure we have interfaces for all major services."""

    def test_all_major_interfaces_exist(self):
        """All major service interfaces are defined."""
        from services import interfaces

        expected = [
            "IPlayerService",
            "IMatchService",
            "IBettingService",
            "ILoanService",
            "IBankruptcyService",
            "IPredictionService",
            "ILobbyService",
            "IGamblingStatsService",
            "IGarnishmentService",
            "IMatchEnrichmentService",
            "IMatchDiscoveryService",
            "IRecalibrationService",
            "IDisburseService",
            "IGuildConfigService",
        ]

        for interface_name in expected:
            assert hasattr(interfaces, interface_name), (
                f"Missing interface: {interface_name}"
            )
            interface = getattr(interfaces, interface_name)
            assert issubclass(interface, ABC), (
                f"{interface_name} should be ABC subclass"
            )
