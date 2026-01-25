"""
Result type for consistent error handling across services.

This module provides a simple Result[T] type that allows services to return
success/failure states without raising exceptions, enabling cleaner error handling
in command layers.

Usage:
    # Returning success
    return Result.ok(data)  # Result with value
    return Result.ok()      # Result without value (for void operations)

    # Returning failure
    return Result.fail("Error message")
    return Result.fail("Error message", code="error_code")

    # Checking results
    if result.success:
        print(result.value)
    else:
        print(f"Error ({result.error_code}): {result.error}")
"""

from dataclasses import dataclass
from typing import Generic, TypeVar

T = TypeVar("T")


@dataclass(frozen=True)
class Result(Generic[T]):
    """
    A simple result type for service method return values.

    Attributes:
        success: Whether the operation succeeded
        value: The return value if successful (None if failed or void operation)
        error: Error message if failed (None if successful)
        error_code: Optional error code for programmatic error handling
    """

    success: bool
    value: T | None = None
    error: str | None = None
    error_code: str | None = None

    @classmethod
    def ok(cls, value: T | None = None) -> "Result[T]":
        """Create a successful result with an optional value."""
        return cls(success=True, value=value)

    @classmethod
    def fail(cls, error: str, code: str | None = None) -> "Result[T]":
        """Create a failed result with an error message and optional error code."""
        return cls(success=False, error=error, error_code=code)

    def __bool__(self) -> bool:
        """Allow using Result in boolean context: if result: ..."""
        return self.success

    def unwrap(self) -> T:
        """
        Get the value, raising ValueError if the result is a failure.

        This is useful when you've already checked success or want to
        propagate errors as exceptions.

        Raises:
            ValueError: If the result is a failure
        """
        if not self.success:
            raise ValueError(f"Cannot unwrap failed result: {self.error}")
        return self.value  # type: ignore

    def unwrap_or(self, default: T) -> T:
        """Get the value or a default if the result is a failure."""
        return self.value if self.success else default  # type: ignore

    def map(self, fn: "callable[[T], Result]") -> "Result":
        """
        Chain operations on successful results.

        If this result is successful, applies fn to the value and returns its result.
        If this result is a failure, returns this failure unchanged.
        """
        if not self.success:
            return self
        return fn(self.value)
