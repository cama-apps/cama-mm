//! Production ntfy.sh push-notification transport.
//!
//! Ntfy topics are user-supplied and the remote host is best-effort, so every
//! failure mode (bad URL, network error, non-2xx response) becomes a typed
//! `NtfyPublishError` for the caller to log; nothing here ever panics or
//! blocks a Discord interaction response on network I/O.

use std::time::Duration;

use reqwest::Client;
use thiserror::Error;

pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum NtfyBuildError {
    #[error("failed to build the ntfy HTTP client: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NtfyPublishError {
    #[error("ntfy server URL must not be empty")]
    EmptyServer,
    #[error("ntfy topic must not be empty")]
    EmptyTopic,
    #[error("ntfy topic must not contain a path separator")]
    InvalidTopic,
    #[error("ntfy publish request failed: {0}")]
    Request(String),
    #[error("ntfy server rejected the publish with status {0}")]
    Rejected(u16),
}

/// Cloneable production ntfy.sh client. `reqwest::Client` is itself a cheap
/// `Arc`-backed handle, so cloning shares one connection pool.
#[derive(Clone, Debug)]
pub struct NtfyHttpClient {
    http: Client,
}

impl NtfyHttpClient {
    pub fn new() -> Result<Self, NtfyBuildError> {
        let http = Client::builder()
            .connect_timeout(REQUEST_TIMEOUT)
            .user_agent("cama-mm-rust/0.1")
            .build()?;
        Ok(Self { http })
    }

    /// Publish one alert. `title` and `message` are sent as ntfy headers/body
    /// exactly as given; ntfy renders them verbatim, so callers are
    /// responsible for any Discord-specific escaping before calling this.
    pub async fn publish(
        &self,
        server: &str,
        topic: &str,
        title: &str,
        message: &str,
    ) -> Result<(), NtfyPublishError> {
        let server = server.trim().trim_end_matches('/');
        let topic = topic.trim();
        if server.is_empty() {
            return Err(NtfyPublishError::EmptyServer);
        }
        if topic.is_empty() {
            return Err(NtfyPublishError::EmptyTopic);
        }
        if topic.contains('/') || topic.contains(char::is_whitespace) {
            return Err(NtfyPublishError::InvalidTopic);
        }

        let url = format!("{server}/{topic}");
        let response = self
            .http
            .post(&url)
            .header("Title", sanitize_header_value(title))
            .header("Priority", "urgent")
            .header("Tags", "rotating_light")
            .timeout(REQUEST_TIMEOUT)
            .body(message.to_owned())
            .send()
            .await
            .map_err(|error| NtfyPublishError::Request(error.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(NtfyPublishError::Rejected(response.status().as_u16()))
        }
    }
}

/// HTTP header values cannot contain control characters (notably newlines);
/// strip them rather than reject the whole publish over cosmetic titles.
fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
#[path = "ntfy_http/tests.rs"]
mod tests;
