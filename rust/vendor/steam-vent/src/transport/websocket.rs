use crate::net::NetworkError;
use bytes::{Bytes, BytesMut};
use futures_util::{Sink, SinkExt, StreamExt, TryStreamExt};
use rustls::{ClientConfig, RootCertStore};
use std::future::ready;
use std::sync::Arc;
use tokio_stream::Stream;
use tokio_tungstenite::tungstenite::{Message as WsMessage, Message, protocol::WebSocketConfig};
use tokio_tungstenite::{Connector, connect_async_tls_with_config};
use tracing::{debug, instrument};

type Result<T, E = NetworkError> = std::result::Result<T, E>;

#[instrument]
pub async fn connect(
    addr: &str,
) -> Result<(
    impl Sink<BytesMut, Error = NetworkError> + use<>,
    impl Stream<Item = Result<BytesMut>> + use<>,
)> {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls_config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| NetworkError::InvalidHeader)?
            .with_root_certificates(root_store)
            .with_no_client_auth();
    let tls_config = Connector::Rustls(Arc::new(tls_config));
    let websocket_config = WebSocketConfig::default()
        .max_message_size(Some(crate::net::MAX_MESSAGE_SIZE))
        .max_frame_size(Some(crate::net::MAX_MESSAGE_SIZE));
    let (stream, _) =
        connect_async_tls_with_config(addr, Some(websocket_config), false, Some(tls_config))
            .await?;
    debug!("connected to websocket server");
    let (raw_write, raw_read) = stream.split();

    Ok((
        raw_write.with(|msg: BytesMut| ready(Ok(WsMessage::binary(msg)))),
        raw_read
            .map_err(NetworkError::from)
            .map_ok(Message::into_data)
            .map_ok(Bytes::from)
            .map_ok(BytesMut::from),
    ))
}
