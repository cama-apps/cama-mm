//! Production ntfy.sh push-notification transport.
//!
//! Production delivery is pinned to the fixed HTTPS ntfy.sh origin. Topics are
//! validated as single safe path segments, redirects are disabled, and every
//! failure becomes a typed `NtfyPublishError` for the caller to log.

use std::time::Duration;

use reqwest::{Client, Url, redirect};
use thiserror::Error;

pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum NtfyBuildError {
    #[error("failed to build the ntfy HTTP client: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid fixed ntfy server URL: {0}")]
    InvalidServer(String),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NtfyPublishError {
    #[error("ntfy topic must not be empty")]
    EmptyTopic,
    #[error("ntfy topic must be at most 64 ASCII letters, numbers, dashes, or underscores")]
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
    server: Url,
}

impl NtfyHttpClient {
    pub fn new() -> Result<Self, NtfyBuildError> {
        Self::for_server(DEFAULT_NTFY_SERVER)
    }

    fn for_server(server: &str) -> Result<Self, NtfyBuildError> {
        let http = Client::builder()
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect(redirect::Policy::none())
            .user_agent("cama-mm-rust/0.1")
            .build()?;
        let server =
            Url::parse(server).map_err(|error| NtfyBuildError::InvalidServer(error.to_string()))?;
        if server.scheme() != "http" && server.scheme() != "https" {
            return Err(NtfyBuildError::InvalidServer(
                "server must use HTTP or HTTPS".to_owned(),
            ));
        }
        if server.cannot_be_a_base() || server.host_str().is_none() {
            return Err(NtfyBuildError::InvalidServer(
                "server must be an absolute hierarchical URL".to_owned(),
            ));
        }
        Ok(Self { http, server })
    }

    /// Publish one alert. `title` and `message` are sent as ntfy headers/body
    /// exactly as given; ntfy renders them verbatim, so callers are
    /// responsible for any Discord-specific escaping before calling this.
    pub async fn publish(
        &self,
        topic: &str,
        title: &str,
        message: &str,
    ) -> Result<(), NtfyPublishError> {
        let topic = topic.trim();
        if topic.is_empty() {
            return Err(NtfyPublishError::EmptyTopic);
        }
        if topic.len() > 64
            || !topic
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(NtfyPublishError::InvalidTopic);
        }

        let mut url = self.server.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.path_segments_mut()
            .map_err(|()| NtfyPublishError::Request("ntfy server cannot host topics".to_owned()))?
            .pop_if_empty()
            .push(topic);
        let response = self
            .http
            .post(url)
            .header("Title", sanitize_header_value(title))
            .header("Priority", "urgent")
            .header("Tags", "rotating_light")
            .timeout(REQUEST_TIMEOUT)
            .body(message.to_owned())
            .send()
            .await
            // The request URL embeds the secret topic; `without_url` drops it
            // so no caller can leak the topic into logs or user-facing copy.
            .map_err(|error| NtfyPublishError::Request(error.without_url().to_string()))?;
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
