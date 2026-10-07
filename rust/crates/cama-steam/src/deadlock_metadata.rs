//! Bounded authoritative Deadlock final-result decoding. A round winner, cache
//! deletion, absent/default protobuf winner, or locator alone cannot settle bets.
use crate::deadlock::{DeadlockGameMode, DeadlockMetadataLocation, DeadlockTeam};
use std::{collections::BTreeMap, io::Read, time::Duration};
use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_deadlock::citadel_gcmessages_common::{
    CMsgMatchMetaData, CMsgMatchMetaDataContents,
};
const MAX_DOWNLOAD: usize = 8 * 1024 * 1024;
const MAX_METADATA: usize = 64 * 1024 * 1024;
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DeadlockMetadataError {
    #[error("Deadlock metadata coordinates are unavailable")]
    Unavailable,
    #[error("Deadlock metadata request failed or is not available yet")]
    Request,
    #[error("Deadlock metadata exceeds its size limit")]
    TooLarge,
    #[error("Deadlock metadata compression is invalid or unsupported")]
    Compression,
    #[error("Deadlock metadata protobuf is invalid")]
    Protobuf,
    #[error("Deadlock metadata does not match the frozen match, roster, teams, or format")]
    WrongMatch,
    #[error("Deadlock metadata does not contain a scored final team victory")]
    NoFinalResult,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadlockFinalResult {
    pub match_id: u64,
    pub winner: DeadlockTeam,
    pub duration_seconds: u32,
    pub start_time: u32,
    pub brawl_rounds: usize,
}
fn metadata_url(location: &DeadlockMetadataLocation) -> Result<String, DeadlockMetadataError> {
    let group = location
        .replay_group_id
        .filter(|id| *id <= 999)
        .ok_or(DeadlockMetadataError::Unavailable)?;
    if location.match_id == 0 {
        return Err(DeadlockMetadataError::Unavailable);
    }
    Ok(format!(
        "https://replay{group}.valve.net/1422450/{}_{}.meta.bz2",
        location.match_id, location.metadata_salt
    ))
}
/// Fetch a Valve locator with fixed numeric URL components, HTTPS and no
/// redirects. CDN/TLS unavailability leaves the market awaiting manual evidence.
pub async fn fetch_final_result(
    location: &DeadlockMetadataLocation,
    mode: DeadlockGameMode,
    expected: Vec<(u32, DeadlockTeam)>,
    created_at: i64,
) -> Result<DeadlockFinalResult, DeadlockMetadataError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| DeadlockMetadataError::Request)?;
    let mut response = client
        .get(metadata_url(location)?)
        .send()
        .await
        .map_err(|_| DeadlockMetadataError::Request)?
        .error_for_status()
        .map_err(|_| DeadlockMetadataError::Request)?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_DOWNLOAD as u64)
    {
        return Err(DeadlockMetadataError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| DeadlockMetadataError::Request)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_DOWNLOAD {
            return Err(DeadlockMetadataError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let match_id = location.match_id;
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| DeadlockMetadataError::Request)?
        .as_secs();
    tokio::task::spawn_blocking(move || {
        let result = decode_final_result(&bytes, match_id, mode, &expected, created_at)?;
        validate_result_time(&result, observed_at)?;
        Ok(result)
    })
    .await
    .map_err(|_| DeadlockMetadataError::Protobuf)?
}
fn validate_result_time(
    result: &DeadlockFinalResult,
    observed_at: u64,
) -> Result<(), DeadlockMetadataError> {
    let latest = observed_at.saturating_add(300);
    if u64::from(result.start_time) > latest
        || u64::from(result.start_time).saturating_add(u64::from(result.duration_seconds)) > latest
    {
        return Err(DeadlockMetadataError::WrongMatch);
    }
    Ok(())
}
fn bounded_read(reader: impl Read) -> Result<Vec<u8>, DeadlockMetadataError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_METADATA as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DeadlockMetadataError::Compression)?;
    if bytes.len() > MAX_METADATA {
        return Err(DeadlockMetadataError::TooLarge);
    }
    Ok(bytes)
}
/// Decode the final overall match result; Street Brawl round outcomes are never
/// used as an alternative to the explicit top-level match winner.
pub fn decode_final_result(
    bytes: &[u8],
    match_id: u64,
    mode: DeadlockGameMode,
    expected: &[(u32, DeadlockTeam)],
    created_at: i64,
) -> Result<DeadlockFinalResult, DeadlockMetadataError> {
    if bytes.len() > MAX_DOWNLOAD {
        return Err(DeadlockMetadataError::TooLarge);
    }
    let decoded = if bytes.starts_with(b"BZh") {
        bounded_read(bzip2::read::BzDecoder::new(bytes))?
    } else if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        bounded_read(
            zstd::stream::read::Decoder::new(bytes)
                .map_err(|_| DeadlockMetadataError::Compression)?,
        )?
    } else {
        return Err(DeadlockMetadataError::Compression);
    };
    let envelope = CMsgMatchMetaData::parse_from_bytes(&decoded)
        .map_err(|_| DeadlockMetadataError::Protobuf)?;
    if envelope.match_id != Some(match_id) || match_id == 0 {
        return Err(DeadlockMetadataError::WrongMatch);
    }
    let details = envelope
        .match_details
        .as_deref()
        .ok_or(DeadlockMetadataError::Protobuf)?;
    let contents = CMsgMatchMetaDataContents::parse_from_bytes(details)
        .map_err(|_| DeadlockMetadataError::Protobuf)?;
    let info = contents
        .match_info
        .as_ref()
        .ok_or(DeadlockMetadataError::NoFinalResult)?;
    if info.match_id != Some(match_id)
        || info.game_mode.map(|v| v.value()) != Some(mode.wire_value())
        || info.match_mode.map(|v| v.value()) != Some(2)
    {
        return Err(DeadlockMetadataError::WrongMatch);
    }
    if info.match_outcome.map(|v| v.value()) != Some(0)
        || info.not_scored == Some(true)
        || info.duration_s.unwrap_or(0) == 0
    {
        return Err(DeadlockMetadataError::NoFinalResult);
    }
    let winner = match info.winning_team.map(|v| v.value()) {
        Some(0) => DeadlockTeam::One,
        Some(1) => DeadlockTeam::Two,
        _ => return Err(DeadlockMetadataError::NoFinalResult),
    };
    let start_time = info
        .start_time
        .ok_or(DeadlockMetadataError::NoFinalResult)?;
    if i64::from(start_time) < created_at.saturating_sub(300) {
        return Err(DeadlockMetadataError::WrongMatch);
    }
    let expected_map: BTreeMap<_, _> = expected
        .iter()
        .map(|(id, team)| (*id, team.wire_value()))
        .collect();
    if expected.len() != mode.player_count()
        || expected_map.len() != expected.len()
        || expected_map.contains_key(&0)
        || expected_map.values().filter(|&&team| team == 0).count() != mode.player_count() / 2
        || info.players.len() != expected.len()
    {
        return Err(DeadlockMetadataError::WrongMatch);
    }
    let mut observed = BTreeMap::new();
    for player in &info.players {
        let account = player.account_id.ok_or(DeadlockMetadataError::WrongMatch)?;
        let team = player
            .team
            .map(|v| v.value())
            .filter(|v| matches!(v, 0 | 1))
            .ok_or(DeadlockMetadataError::WrongMatch)? as u32;
        if observed.insert(account, team).is_some() {
            return Err(DeadlockMetadataError::WrongMatch);
        }
    }
    if observed != expected_map {
        return Err(DeadlockMetadataError::WrongMatch);
    }
    Ok(DeadlockFinalResult {
        match_id,
        winner,
        duration_seconds: info.duration_s.unwrap_or(0),
        start_time,
        brawl_rounds: info.street_brawl_rounds.len(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use steam_vent_proto_common::protobuf::{EnumOrUnknown, MessageField};
    use steam_vent_proto_deadlock::citadel_gcmessages_common::cmsg_match_meta_data_contents::{
        MatchInfo, Players, StreetBrawlRound,
    };
    fn roster() -> Vec<(u32, DeadlockTeam)> {
        (1..=8)
            .map(|id| {
                (
                    id,
                    if id <= 4 {
                        DeadlockTeam::One
                    } else {
                        DeadlockTeam::Two
                    },
                )
            })
            .collect()
    }
    fn info() -> MatchInfo {
        MatchInfo {
            match_id: Some(55),
            game_mode: Some(EnumOrUnknown::from_i32(4)),
            match_mode: Some(EnumOrUnknown::from_i32(2)),
            match_outcome: Some(EnumOrUnknown::from_i32(0)),
            winning_team: Some(EnumOrUnknown::from_i32(1)),
            duration_s: Some(900),
            start_time: Some(1000),
            players: roster()
                .iter()
                .map(|(id, team)| Players {
                    account_id: Some(*id),
                    team: Some(EnumOrUnknown::from_i32(team.wire_value() as i32)),
                    ..Default::default()
                })
                .collect(),
            street_brawl_rounds: vec![StreetBrawlRound {
                winning_team: Some(EnumOrUnknown::from_i32(0)),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    fn bytes(info: MatchInfo, zstd: bool) -> Vec<u8> {
        let details = CMsgMatchMetaDataContents {
            match_info: MessageField::some(info),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap();
        let bytes = CMsgMatchMetaData {
            match_id: Some(55),
            match_details: Some(details),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap();
        if zstd {
            zstd::stream::encode_all(&bytes[..], 1).unwrap()
        } else {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(&bytes).unwrap();
            encoder.finish().unwrap()
        }
    }
    #[test]
    fn both_compressions_use_overall_winner_not_round_winner() {
        for zstd in [false, true] {
            let result = decode_final_result(
                &bytes(info(), zstd),
                55,
                DeadlockGameMode::StreetBrawl,
                &roster(),
                990,
            )
            .unwrap();
            assert_eq!(result.winner, DeadlockTeam::Two);
            assert_eq!(result.brawl_rounds, 1);
        }
    }
    #[test]
    fn missing_winner_draw_unscored_and_round_only_cannot_settle() {
        for modification in 0..4 {
            let mut info = info();
            match modification {
                0 => info.winning_team = None,
                1 => info.match_outcome = Some(EnumOrUnknown::from_i32(2)),
                2 => info.not_scored = Some(true),
                _ => info.match_outcome = None,
            };
            assert_eq!(
                decode_final_result(
                    &bytes(info, false),
                    55,
                    DeadlockGameMode::StreetBrawl,
                    &roster(),
                    990
                ),
                Err(DeadlockMetadataError::NoFinalResult)
            );
        }
    }
    #[test]
    fn rejects_wrong_match_mode_identity_duplicate_and_side() {
        for modification in 0..5 {
            let mut info = info();
            match modification {
                0 => info.match_id = Some(56),
                1 => info.game_mode = Some(EnumOrUnknown::from_i32(1)),
                2 => info.players[0].account_id = Some(99),
                3 => info.players[0].account_id = Some(2),
                _ => info.players[0].team = Some(EnumOrUnknown::from_i32(1)),
            };
            assert_eq!(
                decode_final_result(
                    &bytes(info, true),
                    55,
                    DeadlockGameMode::StreetBrawl,
                    &roster(),
                    990
                ),
                Err(DeadlockMetadataError::WrongMatch)
            );
        }
    }
    #[test]
    fn rejects_old_match_and_bad_compression() {
        assert_eq!(
            decode_final_result(
                &bytes(info(), false),
                55,
                DeadlockGameMode::StreetBrawl,
                &roster(),
                5000
            ),
            Err(DeadlockMetadataError::WrongMatch)
        );
        assert_eq!(
            decode_final_result(
                b"unknown",
                55,
                DeadlockGameMode::StreetBrawl,
                &roster(),
                990
            ),
            Err(DeadlockMetadataError::Compression)
        );
    }
    #[test]
    fn final_result_rejects_future_start_and_future_end_with_clock_tolerance() {
        let mut result = DeadlockFinalResult {
            match_id: 55,
            winner: DeadlockTeam::One,
            duration_seconds: 900,
            start_time: 1000,
            brawl_rounds: 0,
        };
        assert!(validate_result_time(&result, 2000).is_ok());
        result.start_time = 5000;
        assert_eq!(
            validate_result_time(&result, 2000),
            Err(DeadlockMetadataError::WrongMatch)
        );
        result.start_time = 1000;
        result.duration_seconds = 5000;
        assert_eq!(
            validate_result_time(&result, 2000),
            Err(DeadlockMetadataError::WrongMatch)
        );
    }
}
