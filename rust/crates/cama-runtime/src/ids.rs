//! Small helpers shared across the runtime providers.

/// Convert a Discord snowflake into the signed form SQLite stores.
pub fn signed_id(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("Discord {label} ID exceeds SQLite INTEGER"))
}

/// Run a synchronous job on the blocking pool, labelling join failures.
pub async fn blocking<T, F>(label: &'static str, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|error| format!("{label} task failed: {error}"))?
}
