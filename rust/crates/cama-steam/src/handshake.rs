//! Observe GC admission replies that steam-vent's Welcome-only handshake ignores.

use crate::{DOTA_APP_ID, DotaSteamError};
use futures_util::{Stream, StreamExt};
use std::future::Future;
use std::time::Duration;
use steam_vent::{Connection, ConnectionTrait, GameCoordinator, NetworkError};
use steam_vent_proto_common::protobuf::{self, Message};
use steam_vent_proto_common::{RpcMessage, RpcMessageWithKind};
use steam_vent_proto_dota2::dota_gcmessages_client::CMsgClientSuspended;
use steam_vent_proto_dota2::dota_gcmessages_msgid::EDOTAGCMsg;
use steam_vent_proto_dota2::gcsdk_gcmessages::{CMsgClientWelcome, CMsgConnectionStatus};
use steam_vent_proto_dota2::gcsystemmsgs::EGCBaseClientMsg;
use steam_vent_proto_steam::enums_clientserver::EMsg;
use steam_vent_proto_steam::steammessages_clientserver_2::CMsgGCClient;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const PROTO_MASK: u32 = 0x8000_0000;
const CONNECTION_STATUS: u32 = EGCBaseClientMsg::k_EMsgGCClientConnectionStatus as u32;
const CLIENT_SUSPENDED: u32 = EDOTAGCMsg::k_EMsgGCClientSuspended as u32;

pub(crate) async fn connect(
    connection: &Connection,
) -> Result<(GameCoordinator, CMsgClientWelcome), DotaSteamError> {
    // Register before starting the handshake so an immediate rejection is retained.
    let messages = connection.on::<FromGc>();
    let handshake = steam_vent_proto_dota2::GCHandshake::default();
    await_admission(
        connection.game_coordinator(&handshake),
        messages,
        HANDSHAKE_TIMEOUT,
    )
    .await
}

async fn await_admission<T>(
    handshake: impl Future<Output = Result<T, NetworkError>>,
    messages: impl Stream<Item = Result<FromGc, NetworkError>>,
    duration: Duration,
) -> Result<T, DotaSteamError> {
    let deadline = tokio::time::sleep(duration);
    futures_util::pin_mut!(handshake, messages, deadline);
    let mut last_status = None;
    loop {
        tokio::select! {
            result = &mut handshake => return result.map_err(Into::into),
            () = &mut deadline => return Err(DotaSteamError::HandshakeTimeout { last_status }),
            message = messages.next() => {
                let FromGc(message) = message.ok_or(NetworkError::EOF)??;
                if let Some(status) = admission_status(&message)? {
                    last_status = Some(status);
                }
            }
        }
    }
}

fn admission_status(message: &CMsgGCClient) -> Result<Option<i32>, DotaSteamError> {
    if message.appid() != DOTA_APP_ID {
        return Ok(None);
    }
    let kind = message.msgtype() & !PROTO_MASK;
    if kind != CONNECTION_STATUS && kind != CLIENT_SUSPENDED {
        return Ok(None);
    }
    let payload = message.payload();
    if message.msgtype() & PROTO_MASK == 0 || payload.len() < 8 {
        return Err(NetworkError::InvalidHeader.into());
    }
    let header_len = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    let body = payload
        .get(8..)
        .and_then(|rest| rest.get(header_len..))
        .ok_or(NetworkError::InvalidHeader)?;
    if kind == CLIENT_SUSPENDED {
        let suspension =
            CMsgClientSuspended::parse_from_bytes(body).map_err(DotaSteamError::Protobuf)?;
        // The protocol supplies no reason and does not document time_end sentinels.
        // Preserve the raw value; do not infer an account ban or expiration date.
        return Err(DotaSteamError::SessionSuspended {
            time_end: suspension.time_end,
        });
    }
    let status = CMsgConnectionStatus::parse_from_bytes(body).map_err(DotaSteamError::Protobuf)?;
    Ok(status.status.map(|status| status.value()))
}

#[derive(Debug)]
struct FromGc(CMsgGCClient);

impl RpcMessageWithKind for FromGc {
    type KindEnum = EMsg;
    const KIND: EMsg = EMsg::k_EMsgClientFromGC;
}

impl RpcMessage for FromGc {
    fn parse(reader: &mut dyn std::io::Read) -> protobuf::Result<Self> {
        CMsgGCClient::parse_from_reader(reader).map(Self)
    }

    fn write(&self, writer: &mut dyn std::io::Write) -> protobuf::Result<()> {
        self.0.write_to_writer(writer)
    }

    fn encode_size(&self) -> usize {
        self.0.compute_size() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{future, stream};
    use steam_vent_proto_dota2::gcsdk_gcmessages::GCConnectionStatus;

    fn envelope(appid: u32, kind: u32, body: &[u8]) -> CMsgGCClient {
        let mut payload = (kind | PROTO_MASK).to_le_bytes().to_vec();
        payload.extend_from_slice(&0_u32.to_le_bytes());
        payload.extend_from_slice(body);
        CMsgGCClient {
            appid: Some(appid),
            msgtype: Some(kind | PROTO_MASK),
            payload: Some(payload),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn captured_suspension_ends_pending_handshake_without_inferring_a_ban() {
        // Captured CMsgClientSuspended body: field 1 = 2147483647.
        let message = envelope(DOTA_APP_ID, CLIENT_SUSPENDED, &[8, 255, 255, 255, 255, 7]);
        let result = await_admission(
            future::pending::<Result<(), NetworkError>>(),
            stream::once(future::ready(Ok(FromGc(message)))),
            HANDSHAKE_TIMEOUT,
        )
        .await;
        assert!(matches!(
            result,
            Err(DotaSteamError::SessionSuspended {
                time_end: Some(2_147_483_647)
            })
        ));
    }

    #[test]
    fn ignores_other_apps_and_unrelated_messages_before_parsing_payloads() {
        for (appid, kind) in [(730, CLIENT_SUSPENDED), (DOTA_APP_ID, 4004)] {
            let message = CMsgGCClient {
                appid: Some(appid),
                msgtype: Some(kind),
                payload: Some(vec![255]),
                ..Default::default()
            };
            assert_eq!(admission_status(&message).unwrap(), None);
        }
    }

    #[test]
    fn queue_status_is_not_a_rejection_and_unknown_status_is_preserved() {
        for raw_status in [
            GCConnectionStatus::GCConnectionStatus_NO_SESSION_IN_LOGON_QUEUE as i32,
            999,
        ] {
            let status = CMsgConnectionStatus {
                status: Some(protobuf::EnumOrUnknown::from_i32(raw_status)),
                ..Default::default()
            };
            let message = envelope(
                DOTA_APP_ID,
                CONNECTION_STATUS,
                &status.write_to_bytes().unwrap(),
            );
            assert_eq!(admission_status(&message).unwrap(), Some(raw_status));
        }
    }

    #[test]
    fn malformed_header_is_an_error_instead_of_a_panic_or_timeout() {
        let mut message = envelope(DOTA_APP_ID, CLIENT_SUSPENDED, &[]);
        message.payload.as_mut().unwrap()[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            admission_status(&message),
            Err(DotaSteamError::Network(NetworkError::InvalidHeader))
        ));
    }

    #[test]
    fn skips_the_protobuf_header_and_preserves_an_unspecified_suspension_end() {
        let mut message = envelope(DOTA_APP_ID, CLIENT_SUSPENDED, &[]);
        // A non-empty protobuf header is metadata, not the suspension body.
        let payload = message.payload.as_mut().unwrap();
        payload[4..8].copy_from_slice(&2_u32.to_le_bytes());
        payload.extend_from_slice(&[16, 1]);
        assert!(matches!(
            admission_status(&message),
            Err(DotaSteamError::SessionSuspended { time_end: None })
        ));
    }

    #[tokio::test]
    async fn silent_coordinator_has_a_bounded_handshake() {
        let result = await_admission(
            future::pending::<Result<(), NetworkError>>(),
            stream::pending(),
            Duration::ZERO,
        )
        .await;
        assert!(matches!(
            result,
            Err(DotaSteamError::HandshakeTimeout { last_status: None })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_preserves_the_last_queue_status() {
        let queue = GCConnectionStatus::GCConnectionStatus_NO_SESSION_IN_LOGON_QUEUE;
        let status = CMsgConnectionStatus {
            status: Some(queue.into()),
            ..Default::default()
        };
        let message = envelope(
            DOTA_APP_ID,
            CONNECTION_STATUS,
            &status.write_to_bytes().unwrap(),
        );
        let messages = stream::once(future::ready(Ok(FromGc(message)))).chain(stream::pending());
        let start = tokio::time::Instant::now();
        let result = await_admission(
            future::pending::<Result<(), NetworkError>>(),
            messages,
            HANDSHAKE_TIMEOUT,
        )
        .await;
        assert!(
            matches!(result, Err(DotaSteamError::HandshakeTimeout { last_status: Some(value) }) if value == queue as i32)
        );
        assert_eq!(start.elapsed(), HANDSHAKE_TIMEOUT);
    }

    #[tokio::test]
    async fn successful_handshake_returns_its_welcome() {
        let result =
            await_admission(future::ready(Ok(42)), stream::pending(), HANDSHAKE_TIMEOUT).await;
        assert_eq!(result.unwrap(), 42);
    }
}
