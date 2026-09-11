//! Lightweight, flexible WebSockets for Rust.
#![deny(
    missing_docs,
    missing_copy_implementations,
    missing_debug_implementations,
    trivial_casts,
    trivial_numeric_casts,
    unstable_features,
    unused_must_use,
    unused_mut,
    unused_imports,
    unused_import_braces
)]
// This can be removed when `error::Error::Http`, `handshake::HandshakeError::Interrupted` and
// `handshake::server::ErrorResponse` are boxed.
#![allow(clippy::result_large_err)]

#[cfg(feature = "handshake")]
pub use http;

pub mod buffer;
#[cfg(feature = "handshake")]
pub mod client;
pub mod error;
#[cfg(feature = "handshake")]
pub mod handshake;
pub mod protocol;
#[cfg(feature = "handshake")]
mod server;
pub mod stream;
#[cfg(all(any(feature = "native-tls", feature = "__rustls-tls"), feature = "handshake"))]
mod tls;
mod utf8;
pub mod util;

const READ_BUFFER_CHUNK_SIZE: usize = 4096;
type ReadBuffer = buffer::ReadBuffer<READ_BUFFER_CHUNK_SIZE>;

pub use crate::{
    error::{Error, Result},
    protocol::{frame::Utf8Bytes, Message, WebSocket},
};
// re-export bytes since used in `Message` API.
pub use bytes::Bytes;

#[cfg(feature = "handshake")]
pub use crate::{
    client::{client, connect, ClientRequestBuilder},
    handshake::{client::ClientHandshake, server::ServerHandshake, HandshakeError},
    server::{accept, accept_hdr, accept_hdr_with_config, accept_with_config},
};

#[cfg(all(any(feature = "native-tls", feature = "__rustls-tls"), feature = "handshake"))]
pub use tls::{client_tls, client_tls_with_config, Connector};


#[cfg(all(test, feature = "handshake"))]
mod cama_security_tests {
    use super::*;
    use crate::{client::IntoClientRequest, protocol::Role};
    use std::{io::Cursor, sync::Mutex};

    struct Capture(Mutex<Vec<String>>);
    static LOG: Capture = Capture(Mutex::new(Vec::new()));

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool { true }
        fn log(&self, record: &log::Record<'_>) {
            LOG.0.lock().unwrap().push(record.args().to_string());
        }
        fn flush(&self) {}
    }

    #[test]
    fn trace_logs_preserve_events_without_authentication_bytes() {
        log::set_logger(&LOG).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
        let secret = "cama-auth-redaction-regression-sentinel";
        let mut request = "wss://example.invalid/".into_client_request().unwrap();
        request.headers_mut().insert("authorization", secret.parse().unwrap());
        let (wire_request, _) = handshake::client::generate_request(request).unwrap();
        assert!(String::from_utf8(wire_request).unwrap().contains(secret));

        let mut client = WebSocket::from_raw_socket(Cursor::new(Vec::new()), Role::Client, None);
        client.send(Message::Binary(secret.as_bytes().to_vec().into())).unwrap();
        let wire = client.get_ref().get_ref().clone();
        let mut server = WebSocket::from_raw_socket(Cursor::new(wire), Role::Server, None);
        assert_eq!(server.read().unwrap().into_data(), secret.as_bytes());
        let logs = LOG.0.lock().unwrap();
        assert!(logs.iter().any(|line| line.contains("Handshake request:")));
        assert!(logs.iter().any(|line| line.contains("Received message:")));
        let secret_hex: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();
        assert!(logs.iter().all(|line| !line.contains(secret)));
        assert!(logs.iter().all(|line| !line.to_ascii_lowercase().contains(&secret_hex)));
    }
}
