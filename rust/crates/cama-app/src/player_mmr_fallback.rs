//! OpenDota MMR fallback policy and registration seeding.
//!
//! The payload model and fallback curve are transport-independent. Registration
//! consumes a typed OpenDota port so an already-fetched `/players/{id}` payload
//! is reused instead of issuing a second request. Persistence is wired to the
//! existing atomic registration repository; Python remains the live OpenDota
//! HTTP adapter during the additive migration.

use std::fmt::Display;

use cama_db::registration_repository::{
    RegisterPlayerRequest, RegisteredPlayer, RegistrationRepository,
};
use cama_domain::openskill::CamaOpenSkillSystem;
use cama_domain::rating::CamaRatingSystem;
use thiserror::Error;

pub const STEAM_ID64_OFFSET: i64 = 76_561_197_960_265_728;
pub const STEAM_ACCOUNT_ID_UPPER_BOUND: i64 = 1_i64 << 32;

/// A truthy OpenDota MMR scalar. OpenDota has historically returned both JSON
/// numbers and numeric strings for these fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenDotaMmrValue {
    Integer(i64),
    Text(String),
}

impl OpenDotaMmrValue {
    fn is_truthy(&self) -> bool {
        match self {
            Self::Integer(value) => *value != 0,
            Self::Text(value) => !value.is_empty(),
        }
    }

    fn parse(&self) -> Result<i64, MmrParseError> {
        match self {
            Self::Integer(value) => Ok(*value),
            Self::Text(value) => value
                .trim()
                .parse()
                .map_err(|_| MmrParseError::InvalidValue(value.clone())),
        }
    }
}

impl From<i64> for OpenDotaMmrValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<&str> for OpenDotaMmrValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenDotaPlayerData {
    /// True when the source JSON object contained fields outside this focused
    /// projection (or a relevant nested object whose value was null).
    pub has_payload_content: bool,
    pub mmr_estimate: Option<OpenDotaMmrValue>,
    pub computed_mmr: Option<OpenDotaMmrValue>,
    pub solo_competitive_rank: Option<OpenDotaMmrValue>,
    pub rank_tier: Option<i64>,
    pub leaderboard_rank: Option<i64>,
}

impl OpenDotaPlayerData {
    fn is_nonempty_payload(&self) -> bool {
        self.has_payload_content
            || self.mmr_estimate.is_some()
            || self.computed_mmr.is_some()
            || self.solo_competitive_rank.is_some()
            || self.rank_tier.is_some()
            || self.leaderboard_rank.is_some()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MmrParseError {
    #[error("OpenDota returned a non-numeric MMR value: {0}")]
    InvalidValue(String),
}

/// Estimate the approved current-MMR floor for Immortal accounts.
#[must_use]
pub fn estimate_immortal_mmr(leaderboard_rank: Option<i64>) -> i64 {
    let Some(rank) = leaderboard_rank.filter(|rank| *rank > 0) else {
        return 6_000;
    };
    let rank = rank as f64;
    let estimated = 7_000.0 + 1_000.0 * (5_000.0 / rank).log(5.0);
    (estimated.round_ties_even() as i64).clamp(6_000, 12_000)
}

/// Apply the exact OpenDota payload fallback chain without performing I/O.
pub fn get_player_mmr_from_data(
    player_data: Option<&OpenDotaPlayerData>,
) -> Result<Option<i64>, MmrParseError> {
    let Some(player_data) = player_data else {
        return Ok(None);
    };

    let mut mmr = None;
    for candidate in [
        player_data.mmr_estimate.as_ref(),
        player_data.computed_mmr.as_ref(),
        player_data.solo_competitive_rank.as_ref(),
    ] {
        if let Some(candidate) = candidate.filter(|value| value.is_truthy()) {
            mmr = Some(candidate.parse()?);
            break;
        }
    }

    if player_data.rank_tier == Some(80) {
        let floor = estimate_immortal_mmr(player_data.leaderboard_rank);
        Ok(Some(mmr.unwrap_or(0).max(floor)))
    } else {
        Ok(mmr)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegistrationSeed {
    pub discord_id: i64,
    pub discord_username: String,
    pub guild_id: i64,
    pub steam_id: i64,
    pub dotabuff_url: String,
    pub initial_mmr: i64,
    pub glicko_rating: f64,
    pub glicko_rd: f64,
    pub glicko_volatility: f64,
    pub os_mu: f64,
    pub os_sigma: f64,
    pub exclusion_count: i64,
    pub added_at: i64,
}

pub trait MmrRegistrationRepository {
    type Error: Display;

    fn player_exists(&mut self, discord_id: i64, guild_id: i64) -> Result<bool, Self::Error>;
    fn steam_owner(&mut self, steam_id: i64) -> Result<Option<i64>, Self::Error>;
    fn persist_registration(&mut self, seed: &RegistrationSeed) -> Result<(), Self::Error>;
}

impl MmrRegistrationRepository for RegistrationRepository {
    type Error = cama_db::registration_repository::RegistrationRepositoryError;

    fn player_exists(&mut self, discord_id: i64, guild_id: i64) -> Result<bool, Self::Error> {
        RegistrationRepository::player_exists(self, discord_id, Some(guild_id))
    }

    fn steam_owner(&mut self, steam_id: i64) -> Result<Option<i64>, Self::Error> {
        RegistrationRepository::steam_owner(self, steam_id)
    }

    fn persist_registration(&mut self, seed: &RegistrationSeed) -> Result<(), Self::Error> {
        let registered = self.register_player_atomic(&RegisterPlayerRequest {
            discord_id: seed.discord_id,
            guild_id: Some(seed.guild_id),
            discord_username: &seed.discord_username,
            steam_id: seed.steam_id,
            dotabuff_url: &seed.dotabuff_url,
            initial_mmr: seed.initial_mmr,
            glicko_rating: seed.glicko_rating,
            glicko_rd: seed.glicko_rd,
            glicko_volatility: seed.glicko_volatility,
            os_mu: seed.os_mu,
            os_sigma: seed.os_sigma,
            exclusion_count: seed.exclusion_count,
            added_at: seed.added_at,
        })?;
        let RegisteredPlayer {
            discord_id,
            guild_id,
            steam_id,
            initial_mmr,
        } = registered;
        debug_assert_eq!(discord_id, seed.discord_id);
        debug_assert_eq!(guild_id, seed.guild_id);
        debug_assert_eq!(steam_id, seed.steam_id);
        debug_assert_eq!(initial_mmr, seed.initial_mmr);
        Ok(())
    }
}

pub trait OpenDotaRegistrationPort {
    type Error: Display;

    fn get_player_data(&mut self, steam_id: i64)
    -> Result<Option<OpenDotaPlayerData>, Self::Error>;

    /// Derive MMR from the supplied payload. Implementations must not refetch.
    fn get_player_mmr_from_data(
        &mut self,
        player_data: &OpenDotaPlayerData,
    ) -> Result<Option<i64>, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterPlayerInput {
    pub discord_id: i64,
    pub discord_username: String,
    pub guild_id: i64,
    pub steam_id: i64,
    pub mmr_override: Option<i64>,
    pub exclusion_count: i64,
    pub added_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegisterPlayerResult {
    pub cama_rating: i64,
    pub uncertainty: f64,
    pub dotabuff_url: String,
    pub mmr: i64,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RegisterPlayerError {
    #[error("Invalid Steam ID. Must be Steam32 (positive, 32-bit).")]
    InvalidSteamId,
    #[error("Player already registered.")]
    PlayerAlreadyRegistered,
    #[error("Steam ID {steam_id} is already linked to another player.")]
    SteamAlreadyOwned { steam_id: i64 },
    #[error("Could not fetch player data from OpenDota.")]
    PlayerDataUnavailable,
    #[error("OpenDota player lookup failed: {0}")]
    OpenDota(String),
    #[error("MMR not available; please provide your MMR.")]
    MmrUnavailable,
    #[error("player registration persistence failed: {0}")]
    Repository(String),
}

/// Register and seed one player while fetching the OpenDota player payload at
/// most once. The concrete repository adapter performs player creation and the
/// primary Steam link in one `BEGIN IMMEDIATE` transaction.
pub fn register_player<R, A>(
    repository: &mut R,
    api: &mut A,
    input: RegisterPlayerInput,
) -> Result<RegisterPlayerResult, RegisterPlayerError>
where
    R: MmrRegistrationRepository,
    A: OpenDotaRegistrationPort,
{
    if input.steam_id <= 0 || input.steam_id >= STEAM_ACCOUNT_ID_UPPER_BOUND {
        return Err(RegisterPlayerError::InvalidSteamId);
    }
    if repository
        .player_exists(input.discord_id, input.guild_id)
        .map_err(|error| RegisterPlayerError::Repository(error.to_string()))?
    {
        return Err(RegisterPlayerError::PlayerAlreadyRegistered);
    }
    if repository
        .steam_owner(input.steam_id)
        .map_err(|error| RegisterPlayerError::Repository(error.to_string()))?
        .is_some_and(|owner| owner != input.discord_id)
    {
        return Err(RegisterPlayerError::SteamAlreadyOwned {
            steam_id: input.steam_id,
        });
    }

    let mmr = if let Some(mmr) = input.mmr_override {
        Some(mmr)
    } else {
        let player_data = api
            .get_player_data(input.steam_id)
            .map_err(|error| RegisterPlayerError::OpenDota(error.to_string()))?
            .filter(OpenDotaPlayerData::is_nonempty_payload)
            .ok_or(RegisterPlayerError::PlayerDataUnavailable)?;
        let derived = api
            .get_player_mmr_from_data(&player_data)
            .map_err(|error| RegisterPlayerError::OpenDota(error.to_string()))?;
        derived.or_else(|| registration_payload_fallback(&player_data))
    };
    let mmr = mmr
        .filter(|mmr| *mmr > 0)
        .ok_or(RegisterPlayerError::MmrUnavailable)?;

    let rating_system = CamaRatingSystem::default();
    let rating_mmr = mmr.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    let glicko = rating_system.create_player_from_mmr(Some(rating_mmr));
    let seed_mmr = rating_system.new_player_seed_mmr(rating_mmr);
    let openskill = CamaOpenSkillSystem::default();
    let dotabuff_url = format!(
        "https://www.dotabuff.com/players/{}",
        input.steam_id + STEAM_ID64_OFFSET
    );
    let seed = RegistrationSeed {
        discord_id: input.discord_id,
        discord_username: input.discord_username,
        guild_id: input.guild_id,
        steam_id: input.steam_id,
        dotabuff_url: dotabuff_url.clone(),
        initial_mmr: mmr,
        glicko_rating: glicko.rating,
        glicko_rd: glicko.rd,
        glicko_volatility: glicko.volatility,
        os_mu: openskill.mmr_to_os_mu(seed_mmr),
        os_sigma: CamaOpenSkillSystem::DEFAULT_SIGMA,
        exclusion_count: input.exclusion_count,
        added_at: input.added_at,
    };
    repository.persist_registration(&seed).map_err(|error| {
        let message = error.to_string();
        if message.contains("already registered in guild") {
            RegisterPlayerError::PlayerAlreadyRegistered
        } else if message.contains("already linked to player")
            || message.contains("already linked to another player")
        {
            RegisterPlayerError::SteamAlreadyOwned {
                steam_id: input.steam_id,
            }
        } else {
            RegisterPlayerError::Repository(message)
        }
    })?;

    Ok(RegisterPlayerResult {
        cama_rating: rating_system.rating_to_display(glicko.rating),
        uncertainty: rating_system.get_rating_uncertainty_percentage(glicko.rd),
        dotabuff_url,
        mmr,
    })
}

fn registration_payload_fallback(player_data: &OpenDotaPlayerData) -> Option<i64> {
    player_data
        .mmr_estimate
        .as_ref()
        .or(player_data.solo_competitive_rank.as_ref())
        .and_then(|candidate| candidate.parse().ok())
        .filter(|mmr| *mmr > 0)
}
