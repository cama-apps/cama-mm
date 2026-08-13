#[path = "../src/player_mmr_fallback.rs"]
mod player_mmr_fallback;

use std::convert::Infallible;

use player_mmr_fallback::{
    MmrRegistrationRepository, OpenDotaMmrValue, OpenDotaPlayerData, OpenDotaRegistrationPort,
    RegisterPlayerError, RegisterPlayerInput, RegistrationSeed, estimate_immortal_mmr,
    get_player_mmr_from_data, register_player,
};

const TEST_GUILD_ID: i64 = 12_345;

#[derive(Default)]
struct FakeRepo {
    add_calls: Vec<RegistrationSeed>,
}

impl MmrRegistrationRepository for FakeRepo {
    type Error = Infallible;

    fn player_exists(&mut self, _discord_id: i64, _guild_id: i64) -> Result<bool, Self::Error> {
        Ok(false)
    }

    fn steam_owner(&mut self, _steam_id: i64) -> Result<Option<i64>, Self::Error> {
        Ok(None)
    }

    fn persist_registration(&mut self, seed: &RegistrationSeed) -> Result<(), Self::Error> {
        self.add_calls.push(seed.clone());
        Ok(())
    }
}

#[derive(Default)]
struct DummyApi {
    player_data: Option<OpenDotaPlayerData>,
    derived_mmr: Option<i64>,
    player_data_calls: usize,
    mmr_from_data_calls: usize,
}

impl OpenDotaRegistrationPort for DummyApi {
    type Error = Infallible;

    fn get_player_data(
        &mut self,
        _steam_id: i64,
    ) -> Result<Option<OpenDotaPlayerData>, Self::Error> {
        self.player_data_calls += 1;
        Ok(self.player_data.clone())
    }

    fn get_player_mmr_from_data(
        &mut self,
        _player_data: &OpenDotaPlayerData,
    ) -> Result<Option<i64>, Self::Error> {
        self.mmr_from_data_calls += 1;
        Ok(self.derived_mmr)
    }
}

fn data() -> OpenDotaPlayerData {
    OpenDotaPlayerData::default()
}

fn mmr(value: i64) -> OpenDotaMmrValue {
    OpenDotaMmrValue::Integer(value)
}

fn text(value: &str) -> OpenDotaMmrValue {
    OpenDotaMmrValue::Text(value.to_owned())
}

fn assert_payload_mmr(player_data: Option<&OpenDotaPlayerData>, expected: Option<i64>) {
    assert_eq!(get_player_mmr_from_data(player_data).unwrap(), expected);
}

#[test]
fn test_register_player_fallback_to_current_mmr() {
    let mut repo = FakeRepo::default();
    let mut api = DummyApi {
        player_data: Some(OpenDotaPlayerData {
            mmr_estimate: Some(mmr(5_200)),
            ..data()
        }),
        ..DummyApi::default()
    };

    let result = register_player(
        &mut repo,
        &mut api,
        RegisterPlayerInput {
            discord_id: 1,
            discord_username: "user#1".to_owned(),
            guild_id: TEST_GUILD_ID,
            steam_id: 123,
            mmr_override: None,
            exclusion_count: 4,
            added_at: 0,
        },
    )
    .unwrap();

    assert!(!repo.add_calls.is_empty(), "Repo add should be called");
    let added = &repo.add_calls[0];
    assert_eq!(added.initial_mmr, 5_200);
    assert!((added.glicko_rating - 1_175.0).abs() < f64::EPSILON);
    assert!((added.os_mu - 48.5).abs() < f64::EPSILON);
    assert_eq!(result.mmr, 5_200);
    assert_eq!((api.player_data_calls, api.mmr_from_data_calls), (1, 1));
}

#[test]
fn test_register_player_raises_when_no_mmr_anywhere() {
    let mut repo = FakeRepo::default();
    let mut api = DummyApi {
        player_data: Some(OpenDotaPlayerData {
            has_payload_content: true,
            ..OpenDotaPlayerData::default()
        }),
        ..DummyApi::default()
    };

    let error = register_player(
        &mut repo,
        &mut api,
        RegisterPlayerInput {
            discord_id: 2,
            discord_username: "user#2".to_owned(),
            guild_id: TEST_GUILD_ID,
            steam_id: 456,
            mmr_override: None,
            exclusion_count: 4,
            added_at: 0,
        },
    )
    .unwrap_err();

    assert_eq!(error, RegisterPlayerError::MmrUnavailable);
    assert!(repo.add_calls.is_empty());
}

#[test]
fn test_register_player_uses_override() {
    let mut repo = FakeRepo::default();
    let mut api = DummyApi::default();
    let result = register_player(
        &mut repo,
        &mut api,
        RegisterPlayerInput {
            discord_id: 123,
            discord_username: "user#123".to_owned(),
            guild_id: TEST_GUILD_ID,
            steam_id: 42,
            mmr_override: Some(4_000),
            exclusion_count: 4,
            added_at: 0,
        },
    )
    .expect("manual MMR registration");

    assert_eq!(repo.add_calls[0].initial_mmr, 4_000);
    assert_eq!(result.mmr, 4_000);
    assert_eq!((api.player_data_calls, api.mmr_from_data_calls), (0, 0));
}

#[test]
fn test_register_player_requires_mmr_if_no_override() {
    let mut repo = FakeRepo::default();
    let mut api = DummyApi {
        player_data: Some(OpenDotaPlayerData {
            has_payload_content: true,
            ..OpenDotaPlayerData::default()
        }),
        ..DummyApi::default()
    };
    assert_eq!(
        register_player(
            &mut repo,
            &mut api,
            RegisterPlayerInput {
                discord_id: 1,
                discord_username: "u".to_owned(),
                guild_id: TEST_GUILD_ID,
                steam_id: 2,
                mmr_override: None,
                exclusion_count: 4,
                added_at: 0,
            },
        ),
        Err(RegisterPlayerError::MmrUnavailable)
    );
    assert!(repo.add_calls.is_empty());
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data0_5200() {
    let player_data = OpenDotaPlayerData {
        mmr_estimate: Some(text("5200")),
        computed_mmr: Some(mmr(4_800)),
        solo_competitive_rank: Some(mmr(4_200)),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(5_200));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data1_4800() {
    let player_data = OpenDotaPlayerData {
        computed_mmr: Some(text("4800")),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(4_800));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data2_8431() {
    let player_data = OpenDotaPlayerData {
        rank_tier: Some(80),
        leaderboard_rank: Some(500),
        computed_mmr: Some(mmr(4_000)),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(8_431));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data3_8000() {
    let player_data = OpenDotaPlayerData {
        rank_tier: Some(80),
        leaderboard_rank: Some(1_000),
        mmr_estimate: Some(mmr(6_200)),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(8_000));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data4_7000() {
    let player_data = OpenDotaPlayerData {
        rank_tier: Some(80),
        leaderboard_rank: Some(5_000),
        mmr_estimate: Some(mmr(6_800)),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(7_000));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data5_9500() {
    let player_data = OpenDotaPlayerData {
        rank_tier: Some(80),
        leaderboard_rank: Some(200),
        mmr_estimate: Some(mmr(9_500)),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(9_500));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data6_4200() {
    let player_data = OpenDotaPlayerData {
        solo_competitive_rank: Some(text("4200")),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(4_200));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data7_6000() {
    let player_data = OpenDotaPlayerData {
        rank_tier: Some(80),
        ..data()
    };
    assert_payload_mmr(Some(&player_data), Some(6_000));
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_player_data8_none() {
    let player_data = OpenDotaPlayerData::default();
    assert_payload_mmr(Some(&player_data), None);
}

#[test]
fn test_get_player_mmr_from_data_preserves_fallback_chain_none_none() {
    assert_payload_mmr(None, None);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_none_6000() {
    assert_eq!(estimate_immortal_mmr(None), 6_000);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_5000_7000() {
    assert_eq!(estimate_immortal_mmr(Some(5_000)), 7_000);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_1000_8000() {
    assert_eq!(estimate_immortal_mmr(Some(1_000)), 8_000);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_200_9000() {
    assert_eq!(estimate_immortal_mmr(Some(200)), 9_000);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_40_10000() {
    assert_eq!(estimate_immortal_mmr(Some(40)), 10_000);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_8_11000() {
    assert_eq!(estimate_immortal_mmr(Some(8)), 11_000);
}

#[test]
fn test_immortal_mmr_floor_follows_approved_curve_1_12000() {
    assert_eq!(estimate_immortal_mmr(Some(1)), 12_000);
}
