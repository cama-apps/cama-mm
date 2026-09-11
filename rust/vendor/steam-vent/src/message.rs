use crate::net::{MAX_MESSAGE_SIZE, NetMessageHeader, NetworkError, RawNetMessage};
use crate::service_method::ServiceMethodRequest;
use binrw::BinRead;
use byteorder::{LittleEndian, WriteBytesExt};
use bytes::{Buf, BytesMut};
use crc::{CRC_32_ISO_HDLC, Crc};
use flate2::read::GzDecoder;
use futures_util::{
    StreamExt,
    future::ready,
    stream::{iter, once},
};
use num_bigint_dig::ParseBigIntError;
use protobuf::Message;
use std::any::type_name;
use std::fmt::Debug;
use std::io::{Cursor, Read, Write};
use steam_vent_proto_common::{MsgKind, MsgKindEnum, RpcMessage, RpcMessageWithKind};
use steam_vent_proto_steam::enums_clientserver::EMsg;
use steam_vent_proto_steam::steammessages_base::CMsgMulti;
use thiserror::Error;
use tokio_stream::Stream;
use tracing::{debug, trace};

/// Malformed message body
#[derive(Error, Debug)]
#[error("Malformed message body for {0:?}: {1}")]
pub struct MalformedBody(MsgKind, MessageBodyError);

impl MalformedBody {
    pub fn new<K: Into<MsgKind>>(kind: K, err: impl Into<MessageBodyError>) -> Self {
        MalformedBody(kind.into(), err.into())
    }
}

/// Error while parsing the message body
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum MessageBodyError {
    #[error("{0}")]
    Protobuf(#[from] protobuf::Error),
    #[error("{0}")]
    BinRead(#[from] binrw::Error),
    #[error("{0}")]
    IO(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
    #[error("malformed big int: {0:#}")]
    BigInt(#[from] ParseBigIntError),
    #[error("invalid rsa key: {0:#}")]
    Rsa(#[from] rsa::Error),
}

impl From<String> for MessageBodyError {
    fn from(e: String) -> Self {
        MessageBodyError::Other(e)
    }
}

/// A message which can be encoded and/or decoded
///
/// Applications can implement this trait on a struct to allow sending it using
/// [`raw_send_with_kind`](crate::ConnectionTrait::raw_send_with_kind). To use the higher level messages a struct also needs to implement
/// [`NetMessage`]
pub trait EncodableMessage: Sized + Debug + Send {
    fn read_body(_data: BytesMut, _header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        panic!("Reading not implemented for {}", type_name::<Self>())
    }

    fn write_body<W: Write>(&self, _writer: W) -> Result<(), std::io::Error> {
        panic!("Writing not implemented for {}", type_name::<Self>())
    }

    fn encode_size(&self) -> usize {
        panic!("Writing not implemented for {}", type_name::<Self>())
    }

    fn process_header(&self, _header: &mut NetMessageHeader) {}
}

/// A message with associated kind
pub trait NetMessage: EncodableMessage {
    type KindEnum: MsgKindEnum;
    const KIND: Self::KindEnum;
    const IS_PROTOBUF: bool = false;
}

#[derive(Debug, BinRead)]
#[brw(little)]
pub(crate) struct ChannelEncryptRequest {
    pub protocol: u32,
    #[allow(dead_code)]
    pub universe: u32,
    pub nonce: [u8; 16],
}

impl EncodableMessage for ChannelEncryptRequest {
    fn read_body(data: BytesMut, _header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        trace!("reading body of {:?} message", Self::KIND);
        let mut reader = Cursor::new(data);
        ChannelEncryptRequest::read(&mut reader).map_err(|e| MalformedBody::new(Self::KIND, e))
    }
}

impl NetMessage for ChannelEncryptRequest {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgChannelEncryptRequest;
}

#[derive(Debug, BinRead)]
#[brw(little)]
pub(crate) struct ChannelEncryptResult {
    pub result: u32,
}

impl EncodableMessage for ChannelEncryptResult {
    fn read_body(data: BytesMut, _header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        trace!("reading body of {:?} message", Self::KIND);
        let mut reader = Cursor::new(data);
        ChannelEncryptResult::read(&mut reader).map_err(|e| MalformedBody::new(Self::KIND, e))
    }
}

impl NetMessage for ChannelEncryptResult {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgChannelEncryptResult;
}

#[derive(Debug)]
pub(crate) struct ClientEncryptResponse {
    pub protocol: u32,
    pub encrypted_key: Vec<u8>,
}

const CRC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

impl EncodableMessage for ClientEncryptResponse {
    fn write_body<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        trace!("writing body of {:?} message", Self::KIND);
        writer.write_u64::<LittleEndian>(u64::MAX)?;
        writer.write_u64::<LittleEndian>(u64::MAX)?;
        writer.write_u32::<LittleEndian>(self.protocol)?;
        writer.write_u32::<LittleEndian>(self.encrypted_key.len() as u32)?;
        writer.write_all(&self.encrypted_key)?;

        let mut digest = CRC.digest();
        digest.update(&self.encrypted_key);
        writer.write_u32::<LittleEndian>(digest.finalize())?;
        writer.write_u32::<LittleEndian>(0)?;
        Ok(())
    }

    fn encode_size(&self) -> usize {
        8 + 8 + 4 + 4 + self.encrypted_key.len() + 4 + 4
    }
}

impl NetMessage for ClientEncryptResponse {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgChannelEncryptResponse;
}

/// Flatten any "multi" messages in a stream of raw messages
pub(crate) fn flatten_multi<S: Stream<Item = Result<RawNetMessage, NetworkError>>>(
    source: S,
) -> impl Stream<Item = Result<RawNetMessage, NetworkError>> {
    source.flat_map(|res| match res {
        Ok(next) if next.kind == EMsg::k_EMsgMulti => {
            let multi = match MultiBodyIter::new(&next.data) {
                Err(e) => return once(ready(Err(e.into()))).right_stream(),
                Ok(iter) => iter,
            };
            iter(multi).left_stream()
        }
        res => once(ready(res)).right_stream(),
    })
}

// Aggregate inflation is bounded independently from the compressed frame.
const MAX_MULTI_SIZE: usize = 32 * 1024 * 1024;

struct MultiBodyIter {
    data: BytesMut,
}

impl MultiBodyIter {
    pub fn new(encoded: &[u8]) -> Result<Self, MalformedBody> {
        let invalid = || {
            MalformedBody::new(
                EMsg::k_EMsgMulti,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid multi message size",
                ),
            )
        };
        if encoded.len() > MAX_MESSAGE_SIZE {
            return Err(invalid());
        }
        let mut multi = CMsgMulti::parse_from_bytes(encoded)
            .map_err(|e| MalformedBody(EMsg::k_EMsgMulti.into(), e.into()))?;
        let declared = multi.size_unzipped() as usize;
        if declared > MAX_MULTI_SIZE {
            return Err(invalid());
        }
        let body = multi.take_message_body();
        let data = if declared == 0 {
            body
        } else {
            // Read one byte beyond the declaration to detect dishonest sizes. A
            // tiny gzip payload cannot force an unbounded allocation/inflation.
            let mut data = Vec::new();
            GzDecoder::new(body.as_slice())
                .take(declared as u64 + 1)
                .read_to_end(&mut data)
                .map_err(|e| MalformedBody::new(EMsg::k_EMsgMulti, e))?;
            if data.len() != declared {
                return Err(invalid());
            }
            data
        };
        Ok(Self {
            data: BytesMut::from(data.as_slice()),
        })
    }
}

impl Iterator for MultiBodyIter {
    type Item = Result<RawNetMessage, NetworkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.data.is_empty() {
            return None;
        }
        if self.data.len() < 4 {
            self.data.clear();
            return Some(Err(NetworkError::InvalidHeader));
        }
        let size = self.data.get_u32_le() as usize;
        if size < 4 || size > MAX_MESSAGE_SIZE || size > self.data.len() {
            self.data.clear();
            return Some(Err(NetworkError::InvalidHeader));
        }
        // Slice already bounded data; never allocate from the peer's length.
        match RawNetMessage::read(self.data.split_to(size)) {
            Ok(raw) => {
                debug!("Reading child message {:?}", raw.kind);
                Some(Ok(raw))
            }
            Err(e) => {
                self.data.clear();
                Some(Err(e))
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ServiceMethodMessage<Request: Debug>(pub Request);

impl<Request: ServiceMethodRequest + Debug> EncodableMessage for ServiceMethodMessage<Request> {
    fn read_body(data: BytesMut, _header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        trace!("reading body of protobuf message {:?}", Self::KIND);
        Request::parse(&mut data.reader())
            .map_err(|e| MalformedBody::new(Self::KIND, e))
            .map(ServiceMethodMessage)
    }

    fn write_body<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        trace!("writing body of protobuf message {:?}", Self::KIND);
        self.0
            .write(&mut writer)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))
    }

    fn encode_size(&self) -> usize {
        self.0.compute_size() as usize
    }

    fn process_header(&self, header: &mut NetMessageHeader) {
        header.target_job_name = Some(Request::REQ_NAME.into())
    }
}

impl<Request: ServiceMethodRequest + Debug> NetMessage for ServiceMethodMessage<Request> {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgServiceMethodCallFromClient;
    const IS_PROTOBUF: bool = true;
}

#[derive(Debug)]
pub(crate) struct ServiceMethodResponseMessage {
    job_name: String,
    body: BytesMut,
}

impl ServiceMethodResponseMessage {
    pub fn into_response<Request: ServiceMethodRequest>(
        self,
    ) -> Result<Request::Response, NetworkError> {
        if self.job_name == Request::REQ_NAME {
            Ok(Request::Response::parse(&mut self.body.reader())
                .map_err(|e| MalformedBody::new(Self::KIND, e))?)
        } else {
            Err(NetworkError::DifferentServiceMethod(
                Request::REQ_NAME,
                self.job_name,
            ))
        }
    }
}

impl EncodableMessage for ServiceMethodResponseMessage {
    fn read_body(data: BytesMut, header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        trace!("reading body of protobuf message {:?}", Self::KIND);
        Ok(ServiceMethodResponseMessage {
            job_name: header
                .target_job_name
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            body: data,
        })
    }
}

impl NetMessage for ServiceMethodResponseMessage {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgServiceMethodResponse;
    const IS_PROTOBUF: bool = true;
}

#[derive(Debug, Clone)]
pub(crate) struct ServiceMethodNotification {
    pub(crate) job_name: String,
    body: BytesMut,
}

impl ServiceMethodNotification {
    pub fn into_notification<Request: ServiceMethodRequest>(self) -> Result<Request, NetworkError> {
        if self.job_name == Request::REQ_NAME {
            Ok(Request::parse(&mut self.body.reader())
                .map_err(|e| MalformedBody::new(Self::KIND, e))?)
        } else {
            Err(NetworkError::DifferentServiceMethod(
                Request::REQ_NAME,
                self.job_name,
            ))
        }
    }
}

impl EncodableMessage for ServiceMethodNotification {
    fn read_body(data: BytesMut, header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        trace!("reading body of protobuf message {:?}", Self::KIND);
        Ok(ServiceMethodNotification {
            job_name: header
                .target_job_name
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            body: data,
        })
    }
}

impl NetMessage for ServiceMethodNotification {
    type KindEnum = EMsg;
    const KIND: Self::KindEnum = EMsg::k_EMsgServiceMethod;
    const IS_PROTOBUF: bool = true;
}

impl<ProtoMsg: RpcMessageWithKind + Send> EncodableMessage for ProtoMsg {
    fn read_body(data: BytesMut, _header: &NetMessageHeader) -> Result<Self, MalformedBody> {
        trace!("reading body of protobuf message {:?}", Self::KIND);
        Self::parse(&mut data.reader()).map_err(|e| MalformedBody::new(Self::KIND, e))
    }

    fn write_body<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        trace!("writing body of protobuf message {:?}", Self::KIND);
        self.write(&mut writer)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))
    }

    fn encode_size(&self) -> usize {
        <Self as RpcMessage>::encode_size(self)
    }
}

impl<ProtoMsg: RpcMessageWithKind + Send> NetMessage for ProtoMsg {
    type KindEnum = ProtoMsg::KindEnum;
    const KIND: Self::KindEnum = <ProtoMsg as RpcMessageWithKind>::KIND;
    const IS_PROTOBUF: bool = true;
}

#[cfg(test)]
mod multi_tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};

    fn encoded(body: Vec<u8>, size_unzipped: u32) -> Vec<u8> {
        CMsgMulti {
            message_body: Some(body),
            size_unzipped: Some(size_unzipped),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap()
    }

    fn gzip(body: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).unwrap();
        encoder.finish().unwrap()
    }

    fn child() -> Vec<u8> {
        let mut body = 8u32.to_le_bytes().to_vec();
        body.extend_from_slice(&(crate::net::PROTO_MASK | 1).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body
    }

    #[test]
    fn reads_multiple_children_with_and_without_gzip() {
        let body = child().repeat(2);
        for payload in [
            encoded(body.clone(), 0),
            encoded(gzip(&body), body.len() as u32),
        ] {
            let children = MultiBodyIter::new(&payload).unwrap().collect::<Vec<_>>();
            assert_eq!(children.len(), 2);
            assert!(children.iter().all(Result::is_ok));
        }
    }

    #[test]
    fn rejects_gzip_size_mismatch_and_excessive_declared_inflation() {
        let body = child();
        for size in [1, body.len() as u32 - 1, body.len() as u32 + 1, u32::MAX] {
            assert!(MultiBodyIter::new(&encoded(gzip(&body), size)).is_err());
        }
        // Invalid CRC is also rejected, even if the output size matches.
        let mut corrupt = gzip(&body);
        let crc = corrupt.len() - 8;
        corrupt[crc] ^= 1;
        assert!(MultiBodyIter::new(&encoded(corrupt, body.len() as u32)).is_err());
    }

    #[test]
    fn malicious_inner_lengths_and_partial_prefix_fail_once() {
        for body in [
            vec![1],
            vec![1, 2, 3],
            u32::MAX.to_le_bytes().to_vec(),
            0u32.to_le_bytes().to_vec(),
            8u32.to_le_bytes().to_vec(),
        ] {
            let mut iter = MultiBodyIter::new(&encoded(body, 0)).unwrap();
            assert!(iter.next().unwrap().is_err());
            assert!(iter.next().is_none());
        }
    }

    #[test]
    fn truncated_gzip_is_rejected() {
        let body = child();
        let zipped = gzip(&body);
        assert!(
            MultiBodyIter::new(&encoded(
                zipped[..zipped.len() - 1].to_vec(),
                body.len() as u32
            ))
            .is_err()
        );
    }
}
