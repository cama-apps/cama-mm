//! Adapter-neutral orchestration for active mana effects.
//!
//! This module resolves mana effects behind an explicit persistence port. The
//! included in-memory repository makes the lookup executable without opening
//! the live database.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::ops::Deref;

use cama_domain::mana::ManaEffects;

pub type DiscordId = i64;
pub type GuildId = i64;

/// An opaque, read-only view of a resolved effect value.
///
/// `Deref` deliberately has no `DerefMut` counterpart. Callers can inspect the
/// domain fields but cannot mutate the service snapshot in place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManaEffectsSnapshot(ManaEffects);

impl ManaEffectsSnapshot {
    #[must_use]
    pub fn new(effects: ManaEffects) -> Self {
        Self(effects)
    }

    #[must_use]
    pub fn into_inner(self) -> ManaEffects {
        self.0
    }
}

impl Deref for ManaEffectsSnapshot {
    type Target = ManaEffects;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One persisted mana assignment, including the consumed bit from the same
/// read. Reusing that bit prevents a second, inconsistent repository lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManaRecord {
    pub discord_id: DiscordId,
    pub guild_id: GuildId,
    pub land: Option<String>,
    pub color: Option<String>,
    pub assigned_date: String,
    pub consumed: bool,
}

impl ManaRecord {
    #[must_use]
    pub fn new(
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
        land: impl Into<String>,
        assigned_date: impl Into<String>,
    ) -> Self {
        let land = land.into();
        Self {
            discord_id,
            guild_id: normalize_guild_id(guild_id),
            color: color_for_land(&land).map(str::to_owned),
            land: Some(land),
            assigned_date: assigned_date.into(),
            consumed: false,
        }
    }
}

#[must_use]
pub const fn normalize_guild_id(guild_id: Option<GuildId>) -> GuildId {
    match guild_id {
        Some(guild_id) => guild_id,
        None => 0,
    }
}

#[must_use]
pub const fn color_for_land(land: &str) -> Option<&'static str> {
    match land.as_bytes() {
        b"Mountain" => Some("Red"),
        b"Island" => Some("Blue"),
        b"Forest" => Some("Green"),
        b"Plains" => Some("White"),
        b"Swamp" => Some("Black"),
        _ => None,
    }
}

/// Resolve a record against the current 4-AM-Pacific mana-day label.
#[must_use]
pub fn effects_from_record(record: Option<&ManaRecord>, today: &str) -> ManaEffectsSnapshot {
    let Some(record) = record else {
        return ManaEffectsSnapshot::default();
    };
    if record.assigned_date != today || record.consumed {
        return ManaEffectsSnapshot::default();
    }

    let land = record.land.as_deref();
    let color = record
        .color
        .as_deref()
        .or_else(|| land.and_then(color_for_land));
    ManaEffectsSnapshot::new(ManaEffects::for_color(color, land))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryError {
    operation: &'static str,
    message: String,
}

impl RepositoryError {
    #[must_use]
    pub fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.operation, self.message)
    }
}

impl Error for RepositoryError {}

/// Persistence boundary required by mana-effect orchestration.
pub trait ManaEffectsRepository {
    fn current_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<ManaRecord>, RepositoryError>;
}

/// Mana-effect application service with injected state.
pub struct ManaEffectsService<R> {
    repository: R,
    today: String,
}

impl<R: ManaEffectsRepository> ManaEffectsService<R> {
    #[must_use]
    pub fn new(repository: R, today: impl Into<String>) -> Self {
        Self {
            repository,
            today: today.into(),
        }
    }

    #[must_use]
    pub const fn repository(&self) -> &R {
        &self.repository
    }

    pub fn repository_mut(&mut self) -> &mut R {
        &mut self.repository
    }

    #[must_use]
    pub fn into_repository(self) -> R {
        self.repository
    }

    pub fn get_effects(
        &mut self,
        discord_id: DiscordId,
        guild_id: Option<GuildId>,
    ) -> Result<ManaEffectsSnapshot, RepositoryError> {
        let record = self
            .repository
            .current_mana(discord_id, normalize_guild_id(guild_id))?;
        Ok(effects_from_record(record.as_ref(), &self.today))
    }
}

/// Reference adapter used by parity tests and non-production simulations.
#[derive(Clone, Debug, Default)]
pub struct InMemoryManaEffectsRepository {
    mana: BTreeMap<(GuildId, DiscordId), ManaRecord>,
    current_mana_reads: usize,
}

impl InMemoryManaEffectsRepository {
    pub fn assign_mana(&mut self, record: ManaRecord) {
        self.mana
            .insert((record.guild_id, record.discord_id), record);
    }

    #[must_use]
    pub const fn current_mana_reads(&self) -> usize {
        self.current_mana_reads
    }
}

impl ManaEffectsRepository for InMemoryManaEffectsRepository {
    fn current_mana(
        &mut self,
        discord_id: DiscordId,
        guild_id: GuildId,
    ) -> Result<Option<ManaRecord>, RepositoryError> {
        self.current_mana_reads += 1;
        Ok(self.mana.get(&(guild_id, discord_id)).cloned())
    }
}

#[cfg(test)]
#[path = "mana_effects_service/tests.rs"]
mod tests;
