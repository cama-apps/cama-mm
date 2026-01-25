"""Tests for the Result type and error codes."""

import pytest

from services.result import Result
from services import error_codes


class TestResultOk:
    """Tests for successful Result creation."""

    def test_ok_without_value(self):
        """Result.ok() creates success without value."""
        result = Result.ok()
        assert result.success is True
        assert result.value is None
        assert result.error is None
        assert result.error_code is None

    def test_ok_with_value(self):
        """Result.ok(value) creates success with value."""
        result = Result.ok(42)
        assert result.success is True
        assert result.value == 42
        assert result.error is None
        assert result.error_code is None

    def test_ok_with_dict_value(self):
        """Result.ok() works with dict values."""
        data = {"player": "test", "rating": 1500}
        result = Result.ok(data)
        assert result.success is True
        assert result.value == data
        assert result.value["player"] == "test"

    def test_ok_with_none_value(self):
        """Result.ok(None) is valid for void operations."""
        result = Result.ok(None)
        assert result.success is True
        assert result.value is None


class TestResultFail:
    """Tests for failed Result creation."""

    def test_fail_with_message(self):
        """Result.fail(msg) creates failure with message."""
        result = Result.fail("Something went wrong")
        assert result.success is False
        assert result.value is None
        assert result.error == "Something went wrong"
        assert result.error_code is None

    def test_fail_with_code(self):
        """Result.fail(msg, code) creates failure with code."""
        result = Result.fail("Player not found", code=error_codes.PLAYER_NOT_FOUND)
        assert result.success is False
        assert result.error == "Player not found"
        assert result.error_code == error_codes.PLAYER_NOT_FOUND

    def test_fail_with_custom_code(self):
        """Result.fail works with custom error codes."""
        result = Result.fail("Custom error", code="custom_error")
        assert result.success is False
        assert result.error_code == "custom_error"


class TestResultBooleanContext:
    """Tests for Result in boolean context."""

    def test_ok_is_truthy(self):
        """Successful result is truthy."""
        result = Result.ok(42)
        assert bool(result) is True
        # Also works in if statements
        if result:
            passed = True
        else:
            passed = False
        assert passed is True

    def test_fail_is_falsy(self):
        """Failed result is falsy."""
        result = Result.fail("error")
        assert bool(result) is False
        # Also works in if statements
        if result:
            passed = True
        else:
            passed = False
        assert passed is False


class TestResultUnwrap:
    """Tests for Result.unwrap() and unwrap_or()."""

    def test_unwrap_success(self):
        """unwrap() returns value on success."""
        result = Result.ok(42)
        assert result.unwrap() == 42

    def test_unwrap_failure_raises(self):
        """unwrap() raises ValueError on failure."""
        result = Result.fail("Something went wrong")
        with pytest.raises(ValueError, match="Cannot unwrap failed result"):
            result.unwrap()

    def test_unwrap_or_success(self):
        """unwrap_or() returns value on success."""
        result = Result.ok(42)
        assert result.unwrap_or(0) == 42

    def test_unwrap_or_failure(self):
        """unwrap_or() returns default on failure."""
        result = Result.fail("error")
        assert result.unwrap_or(0) == 0


class TestResultMap:
    """Tests for Result.map() chaining."""

    def test_map_on_success(self):
        """map() applies function on success."""
        result = Result.ok(5)
        mapped = result.map(lambda x: Result.ok(x * 2))
        assert mapped.success is True
        assert mapped.value == 10

    def test_map_on_failure(self):
        """map() returns original failure."""
        result = Result.fail("error", code="test_error")
        mapped = result.map(lambda x: Result.ok(x * 2))
        assert mapped.success is False
        assert mapped.error == "error"
        assert mapped.error_code == "test_error"

    def test_map_chain(self):
        """map() can be chained."""
        result = (
            Result.ok(5)
            .map(lambda x: Result.ok(x * 2))
            .map(lambda x: Result.ok(x + 1))
        )
        assert result.value == 11


class TestResultImmutability:
    """Tests for Result immutability."""

    def test_result_is_frozen(self):
        """Result is immutable (frozen dataclass)."""
        result = Result.ok(42)
        with pytest.raises(Exception):  # FrozenInstanceError
            result.value = 100


class TestErrorCodes:
    """Tests for error code constants."""

    def test_error_codes_are_strings(self):
        """All error codes are strings."""
        codes = [
            error_codes.NOT_FOUND,
            error_codes.VALIDATION_ERROR,
            error_codes.INSUFFICIENT_FUNDS,
            error_codes.COOLDOWN_ACTIVE,
            error_codes.PLAYER_NOT_FOUND,
            error_codes.LOBBY_FULL,
            error_codes.BETTING_CLOSED,
        ]
        for code in codes:
            assert isinstance(code, str)

    def test_error_codes_are_unique(self):
        """Error codes should be unique."""
        import inspect

        # Get all public constants from error_codes module
        codes = [
            value
            for name, value in inspect.getmembers(error_codes)
            if not name.startswith("_") and isinstance(value, str)
        ]
        # Check uniqueness
        assert len(codes) == len(set(codes)), "Duplicate error codes found"

    def test_common_error_codes_exist(self):
        """Common error codes are defined."""
        assert hasattr(error_codes, "NOT_FOUND")
        assert hasattr(error_codes, "VALIDATION_ERROR")
        assert hasattr(error_codes, "INSUFFICIENT_FUNDS")
        assert hasattr(error_codes, "COOLDOWN_ACTIVE")
        assert hasattr(error_codes, "STATE_ERROR")


class TestResultTyping:
    """Tests for Result type hints."""

    def test_typed_result_string(self):
        """Result[str] works with string values."""
        result: Result[str] = Result.ok("hello")
        assert result.value == "hello"

    def test_typed_result_int(self):
        """Result[int] works with int values."""
        result: Result[int] = Result.ok(42)
        assert result.value == 42

    def test_typed_result_dict(self):
        """Result[dict] works with dict values."""
        result: Result[dict] = Result.ok({"key": "value"})
        assert result.value["key"] == "value"

    def test_typed_result_list(self):
        """Result[list] works with list values."""
        result: Result[list] = Result.ok([1, 2, 3])
        assert result.value == [1, 2, 3]
