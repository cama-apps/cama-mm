"""Tests for the Result type and error codes."""

import pytest

from services.result import Result


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
