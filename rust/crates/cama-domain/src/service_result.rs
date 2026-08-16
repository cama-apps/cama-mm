//! Result type for consistent error handling across services.
//!
//! This mirrors the Python service result contract while using Rust's type
//! system to distinguish successes from failures. Value-less operations use
//! `ServiceResult<()>` and succeed with `ServiceResult::ok(())`.

/// The outcome of a service operation.
///
/// A success always owns its value. A failure always owns an error message and
/// may additionally carry a stable error code for programmatic handling.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "service results must be checked or explicitly discarded"]
pub enum ServiceResult<T> {
    /// The operation completed successfully.
    Success(T),
    /// The operation failed without raising an exception.
    Failure {
        /// Human-readable failure detail.
        error: String,
        /// Optional stable code for callers that should not parse `error`.
        error_code: Option<String>,
    },
}

impl<T> ServiceResult<T> {
    /// Create a successful service result.
    pub const fn ok(value: T) -> Self {
        Self::Success(value)
    }

    /// Create a failed service result without an error code.
    pub fn fail(error: impl Into<String>) -> Self {
        Self::Failure {
            error: error.into(),
            error_code: None,
        }
    }

    /// Create a failed service result with an error code.
    pub fn fail_with_code(error: impl Into<String>, error_code: impl Into<String>) -> Self {
        Self::Failure {
            error: error.into(),
            error_code: Some(error_code.into()),
        }
    }

    /// Whether the operation completed successfully.
    ///
    /// This is the explicit Rust equivalent of using the Python result in a
    /// boolean context.
    pub const fn success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    /// Whether the operation completed successfully.
    pub const fn is_success(&self) -> bool {
        self.success()
    }

    /// Borrow the success value, or return `None` for a failure.
    pub const fn value(&self) -> Option<&T> {
        match self {
            Self::Success(value) => Some(value),
            Self::Failure { .. } => None,
        }
    }

    /// Borrow the error message, or return `None` for a success.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Success(_) => None,
            Self::Failure { error, .. } => Some(error),
        }
    }

    /// Borrow the error code, if this is a coded failure.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Success(_) => None,
            Self::Failure { error_code, .. } => error_code.as_deref(),
        }
    }

    /// Return the success value, panicking when this result is a failure.
    ///
    /// Callers that do not want a panic should inspect [`Self::success`] or use
    /// [`Self::unwrap_or`].
    #[track_caller]
    pub fn unwrap(self) -> T {
        match self {
            Self::Success(value) => value,
            Self::Failure { error, .. } => {
                panic!("Cannot unwrap failed result: {error}")
            }
        }
    }

    /// Return the success value, or `default` when this result is a failure.
    pub fn unwrap_or(self, default: T) -> T {
        match self {
            Self::Success(value) => value,
            Self::Failure { .. } => default,
        }
    }

    /// Chain a service operation onto a successful result.
    ///
    /// Failures skip `operation` and retain their message and error code. The
    /// result is consumed so a successful operation may change the value type.
    pub fn map<U>(self, operation: impl FnOnce(T) -> ServiceResult<U>) -> ServiceResult<U> {
        match self {
            Self::Success(value) => operation(value),
            Self::Failure { error, error_code } => ServiceResult::Failure { error, error_code },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ServiceResult;

    #[test]
    fn test_ok_is_truthy() {
        let result = ServiceResult::ok(42);

        assert!(result.success());
        if !result.success() {
            panic!("a successful result must take the success branch");
        }
    }

    #[test]
    fn test_fail_is_falsy() {
        let result = ServiceResult::<i32>::fail("error");

        assert!(!result.success());
        if result.success() {
            panic!("a failed result must not take the success branch");
        }
    }

    #[test]
    fn test_unwrap_success() {
        let result = ServiceResult::ok(42);

        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    #[should_panic(expected = "Cannot unwrap failed result")]
    fn test_unwrap_failure_raises() {
        let result = ServiceResult::<i32>::fail("Something went wrong");

        result.unwrap();
    }

    #[test]
    fn test_unwrap_or_success() {
        let result = ServiceResult::ok(42);

        assert_eq!(result.unwrap_or(0), 42);
    }

    #[test]
    fn test_unwrap_or_failure() {
        let result = ServiceResult::<i32>::fail("error");

        assert_eq!(result.unwrap_or(0), 0);
    }

    #[test]
    fn test_map_on_success() {
        let result = ServiceResult::ok(5);
        let mapped = result.map(|value| ServiceResult::ok(value * 2));

        assert!(mapped.success());
        assert_eq!(mapped.value(), Some(&10));
    }

    #[test]
    fn test_map_on_failure() {
        let result = ServiceResult::<i32>::fail_with_code("error", "test_error");
        let mapped = result.map(|value| ServiceResult::ok(value * 2));

        assert!(!mapped.success());
        assert_eq!(mapped.error(), Some("error"));
        assert_eq!(mapped.error_code(), Some("test_error"));
    }

    #[test]
    fn test_map_chain() {
        let result = ServiceResult::ok(5)
            .map(|value| ServiceResult::ok(value * 2))
            .map(|value| ServiceResult::ok(value + 1));

        assert_eq!(result.value(), Some(&11));
    }
}
