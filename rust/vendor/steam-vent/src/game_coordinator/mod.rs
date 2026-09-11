pub mod handshake;

use crate::connection::{ConnectionImpl, ConnectionTrait, MessageFilter, MessageSender};
use crate::message::EncodableMessage;
use crate::net::{JobId, NetMessageHeader, RawNetMessage, decode_kind};
use crate::session::Session;
use crate::{Connection, NetMessage, NetworkError};
use futures_util::future::{Either, select};
use protobuf::Message;
use std::fmt::{Debug, Formatter};
use std::pin::pin;
use std::time::Duration;
use steam_vent_proto_common::{GCHandshake, MsgKindEnum, RpcMessage, RpcMessageWithKind};
use steam_vent_proto_steam::enums_clientserver::EMsg;
use steam_vent_proto_steam::steammessages_clientserver::CMsgClientGamesPlayed;
use steam_vent_proto_steam::steammessages_clientserver::cmsg_client_games_played::GamePlayed;
use steam_vent_proto_steam::steammessages_clientserver_2::CMsgGCClient;
use steam_vent_proto_steam::steammessages_clientserver_login::CMsgClientHello;
use tokio::time::sleep;
use tokio_stream::StreamExt;
use tracing::debug;

pub struct GameCoordinator {
    app_id: u32,
    filter: MessageFilter,
    sender: MessageSender,
    session: Session,
    timeout: Duration,
}

/// While these kinds are consistent between games, they are not defined in the generic steam protobufs.
/// We define them here, so we can implement the game coordinator without requiring the protobufs from a game
#[repr(i32)]
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub enum GCMsgKind {
    #[default]
    Invalid = 0,
    k_EMsgGCClientWelcome = 4004,
    k_EMsgGCServerWelcome = 4005,
    k_EMsgGCClientHello = 4006,
    k_EMsgGCServerHello = 4007,
}

impl protobuf::Enum for GCMsgKind {
    const NAME: &'static str = "GCMsgKind";

    fn value(&self) -> i32 {
        *self as i32
    }

    fn from_i32(v: i32) -> Option<Self> {
        match v {
            4004 => Some(Self::k_EMsgGCClientWelcome),
            4005 => Some(Self::k_EMsgGCServerWelcome),
            4006 => Some(Self::k_EMsgGCClientHello),
            4007 => Some(Self::k_EMsgGCServerHello),
            _ => None,
        }
    }

    fn from_str(_s: &str) -> Option<Self> {
        None
    }
}

impl MsgKindEnum for GCMsgKind {}

impl Debug for GameCoordinator {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameCoordinator")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl GameCoordinator {
    /// Create new `GameCoordinator` with the default handshake
    pub async fn new(connection: &Connection, app_id: u32) -> Result<Self, NetworkError> {
        let (gc, _) = Self::init_raw(connection, app_id, CMsgClientHello::default).await?;
        Ok(gc)
    }

    /// Create new `GameCoordinator` instance returning the received welcome message.
    pub async fn with_welcome<Welcome: NetMessage>(
        connection: &Connection,
        app_id: u32,
    ) -> Result<(Self, Welcome), NetworkError> {
        let (gc, welcome) = Self::init_raw(connection, app_id, CMsgClientHello::default).await?;
        Ok((gc, welcome.into_message()?))
    }

    /// Create new `GameCoordinator` instance with a custom handshake
    pub async fn with_handshake<Handshake: GCHandshake>(
        connection: &Connection,
        handshake: &Handshake,
    ) -> Result<(Self, Handshake::Welcome), NetworkError> {
        let (gc, welcome) =
            Self::init_raw(connection, handshake.app_id(), || handshake.hello()).await?;
        Ok((gc, welcome.into_message()?))
    }

    async fn init_raw<HelloMsg: NetMessage, HelloFn: Fn() -> HelloMsg>(
        connection: &Connection,
        app_id: u32,
        hello_msg: HelloFn,
    ) -> Result<(Self, RawNetMessage), NetworkError> {
        // Keep forwarding inside the filter-owned stream so dropping the GC
        // cancels its subscription immediately, including during an idle match.
        let gc_messages = connection
            .on::<ClientFromGcMessage>()
            .filter_map(move |message| decode_gc_message(app_id, message));
        let filter = MessageFilter::new(gc_messages);

        let gc = GameCoordinator {
            app_id,
            filter,
            sender: connection.sender().clone(),
            session: connection.session().clone().with_app_id(app_id),
            timeout: connection.timeout(),
        };

        connection
            .send_with_kind(
                CMsgClientGamesPlayed {
                    games_played: vec![GamePlayed {
                        game_id: Some(app_id as u64),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                EMsg::k_EMsgClientGamesPlayedWithDataBlob,
            )
            .await?;

        let welcome = gc.wait_welcome();
        let hello_sender = async {
            loop {
                if let Err(e) = gc.send_hello(&hello_msg).await {
                    return Result::<(), _>::Err(e);
                };
                sleep(Duration::from_secs(5)).await;
            }
        };

        let welcome = match select(pin!(welcome), pin!(hello_sender)).await {
            Either::Left((welcome, _)) => welcome?,
            Either::Right((hello_sender, _)) => {
                return Err(hello_sender.expect_err("unreachable: unexpected Ok from hello_sender"));
            }
        };
        Ok((gc, welcome))
    }

    async fn send_hello<HelloMsg: NetMessage, HelloFn: Fn() -> HelloMsg>(
        &self,
        hello_fn: HelloFn,
    ) -> Result<(), NetworkError> {
        if self.session.is_server() {
            self.send_with_kind(hello_fn(), GCMsgKind::k_EMsgGCServerHello)
                .await?;
        } else {
            self.send_with_kind(hello_fn(), GCMsgKind::k_EMsgGCClientHello)
                .await?;
        }
        Ok(())
    }

    async fn wait_welcome(&self) -> Result<RawNetMessage, NetworkError> {
        if self.session.is_server() {
            self.filter.one_kind(GCMsgKind::k_EMsgGCServerWelcome)
        } else {
            self.filter.one_kind(GCMsgKind::k_EMsgGCClientWelcome)
        }
        .await
        .map_err(|_| NetworkError::EOF)
    }
}

impl ConnectionImpl for GameCoordinator {
    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn filter(&self) -> &MessageFilter {
        &self.filter
    }

    fn session(&self) -> &Session {
        &self.session
    }

    async fn raw_send_with_kind<Msg: EncodableMessage, K: MsgKindEnum>(
        &self,
        mut header: NetMessageHeader,
        msg: Msg,
        kind: K,
        is_protobuf: bool,
    ) -> Result<(), NetworkError> {
        let nested_header = NetMessageHeader {
            source_job_id: header.source_job_id,
            ..Default::default()
        };
        header.source_job_id = JobId::default();

        let mut payload: Vec<u8> = Vec::with_capacity(
            nested_header.encode_size(kind.into(), is_protobuf) + msg.encode_size(),
        );

        nested_header.write(&mut payload, kind, is_protobuf)?;
        msg.write_body(&mut payload)?;
        let data = CMsgGCClient {
            appid: Some(self.app_id),
            msgtype: Some(kind.encode_kind(is_protobuf)),
            payload: Some(payload),
            ..Default::default()
        };

        let msg = RawNetMessage::from_message(header, ClientToGcMessage { data })?;
        self.sender.send_raw(msg).await
    }
}

#[derive(Debug)]
struct ClientToGcMessage {
    data: CMsgGCClient,
}

impl RpcMessageWithKind for ClientToGcMessage {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgClientToGC;
}

impl RpcMessage for ClientToGcMessage {
    fn parse(reader: &mut dyn std::io::Read) -> protobuf::Result<Self> {
        let data = <CMsgGCClient as Message>::parse_from_reader(reader)?;
        Ok(ClientToGcMessage { data })
    }
    fn write(&self, writer: &mut dyn std::io::Write) -> protobuf::Result<()> {
        self.data.write_to_writer(writer)
    }
    fn encode_size(&self) -> usize {
        self.data.compute_size() as usize
    }
}

#[derive(Debug)]
struct ClientFromGcMessage {
    data: CMsgGCClient,
}

impl RpcMessageWithKind for ClientFromGcMessage {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgClientFromGC;
}

impl RpcMessage for ClientFromGcMessage {
    fn parse(reader: &mut dyn std::io::Read) -> protobuf::Result<Self> {
        let data = <CMsgGCClient as Message>::parse_from_reader(reader)?;
        Ok(ClientFromGcMessage { data })
    }
    fn write(&self, writer: &mut dyn std::io::Write) -> protobuf::Result<()> {
        self.data.write_to_writer(writer)
    }
    fn encode_size(&self) -> usize {
        self.data.compute_size() as usize
    }
}

fn decode_gc_message(
    app_id: u32,
    message: Result<ClientFromGcMessage, NetworkError>,
) -> Option<Result<RawNetMessage, NetworkError>> {
    match message {
        Ok(mut message) if message.data.appid() == app_id => {
            let (kind, is_protobuf) = decode_kind(message.data.msgtype());
            debug!(kind = ?kind, is_protobuf, "received gc messages");
            Some(
                RawNetMessage::read(message.data.take_payload()).and_then(|raw| {
                    if raw.kind != kind || raw.is_protobuf != is_protobuf {
                        Err(NetworkError::InvalidHeader)
                    } else {
                        Ok(raw)
                    }
                }),
            )
        }
        Ok(_) => None,
        Err(error) => Some(Err(error)),
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;

    fn message(appid: u32, kind: u32) -> ClientFromGcMessage {
        ClientFromGcMessage {
            data: CMsgGCClient {
                appid: Some(appid),
                msgtype: Some(kind),
                payload: Some(vec![1, 0, 0, 128, 0, 0, 0, 0]),
                ..Default::default()
            },
        }
    }

    #[test]
    fn ignores_other_games_and_rejects_kind_mismatch() {
        let kind = crate::net::PROTO_MASK | 1;
        assert!(decode_gc_message(570, Ok(message(730, kind))).is_none());
        assert!(
            decode_gc_message(570, Ok(message(570, kind)))
                .unwrap()
                .is_ok()
        );
        assert!(
            decode_gc_message(570, Ok(message(570, kind + 1)))
                .unwrap()
                .is_err()
        );
        assert!(
            decode_gc_message(570, Ok(message(570, 1)))
                .unwrap()
                .is_err()
        );
    }
}
