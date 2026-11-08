//! Typed Dig relic rarity and recycling policy independent from Discord.
//!
//! This module reuses the Dig artifact catalog, injects entropy, and makes persistence a port. The
//! SQLite recycling adapter revalidates inventory after application-level checks so stale
//! selections cannot partially consume relics.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Display;

use cama_db::dig_relic_recycling::{
    DigRelicRecyclingRepository, DigRelicRecyclingRepositoryError, PersistedRelicRow, RelicOwner,
};

use crate::dig_loot::{ArtifactDef, Rarity, artifact_catalog};
use crate::dig_prestige4_content;

pub const RELIC_RARITY_ORDER: [Rarity; 4] = [
    Rarity::Common,
    Rarity::Uncommon,
    Rarity::Rare,
    Rarity::Legendary,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedRelicRow {
    pub row_id: i64,
    pub artifact_id: String,
    pub is_relic: bool,
    pub equipped: bool,
}

impl From<PersistedRelicRow> for OwnedRelicRow {
    fn from(row: PersistedRelicRow) -> Self {
        Self {
            row_id: row.row_id,
            artifact_id: row.artifact_id,
            is_relic: row.is_relic,
            equipped: row.equipped,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecycleRelicsOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub source_rarity: Option<Rarity>,
    pub output_rarity: Option<Rarity>,
    pub relic_id: Option<&'static str>,
    pub relic_name: Option<&'static str>,
    pub artifact_row_id: Option<i64>,
}

impl RecycleRelicsOutcome {
    fn rejected(error: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(error.into()),
            source_rarity: None,
            output_rarity: None,
            relic_id: None,
            relic_name: None,
            artifact_row_id: None,
        }
    }

    fn recycled(source_rarity: Rarity, output: ArtifactDef, artifact_row_id: i64) -> Self {
        Self {
            success: true,
            error: None,
            source_rarity: Some(source_rarity),
            output_rarity: Some(output.rarity),
            relic_id: Some(output.id),
            relic_name: Some(output.name),
            artifact_row_id: Some(artifact_row_id),
        }
    }
}

pub trait RelicEntropy {
    fn unit(&mut self) -> f64;
    fn choose_index(&mut self, length: usize) -> usize;
}

#[derive(Clone, Debug)]
pub struct ScriptedRelicEntropy {
    units: VecDeque<f64>,
    indexes: VecDeque<usize>,
    fallback_unit: f64,
    fallback_index: usize,
}

impl ScriptedRelicEntropy {
    #[must_use]
    pub fn new(
        units: impl IntoIterator<Item = f64>,
        indexes: impl IntoIterator<Item = usize>,
    ) -> Self {
        Self {
            units: units.into_iter().collect(),
            indexes: indexes.into_iter().collect(),
            fallback_unit: 1.0,
            fallback_index: 0,
        }
    }

    #[must_use]
    pub fn choosing(index: usize) -> Self {
        Self {
            units: VecDeque::new(),
            indexes: VecDeque::new(),
            fallback_unit: 1.0,
            fallback_index: index,
        }
    }
}

impl RelicEntropy for ScriptedRelicEntropy {
    fn unit(&mut self) -> f64 {
        self.units.pop_front().unwrap_or(self.fallback_unit)
    }

    fn choose_index(&mut self, length: usize) -> usize {
        self.indexes
            .pop_front()
            .unwrap_or(self.fallback_index)
            .min(length.saturating_sub(1))
    }
}

pub trait RelicRecyclePort {
    type Error: Display;

    fn owned_relic_rows(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<OwnedRelicRow>, Self::Error>;
    fn atomic_recycle(
        &self,
        discord_id: i64,
        guild_id: i64,
        selected_row_ids: &[i64],
        output_artifact_id: &str,
        found_at: i64,
    ) -> Result<i64, Self::Error>;
}

impl RelicRecyclePort for DigRelicRecyclingRepository {
    type Error = DigRelicRecyclingRepositoryError;

    fn owned_relic_rows(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Vec<OwnedRelicRow>, Self::Error> {
        self.relic_rows(RelicOwner {
            discord_id,
            guild_id: Some(guild_id),
        })
        .map(|rows| rows.into_iter().map(Into::into).collect())
    }

    fn atomic_recycle(
        &self,
        discord_id: i64,
        guild_id: i64,
        selected_row_ids: &[i64],
        output_artifact_id: &str,
        found_at: i64,
    ) -> Result<i64, Self::Error> {
        self.atomic_recycle_relics(
            RelicOwner {
                discord_id,
                guild_id: Some(guild_id),
            },
            selected_row_ids,
            output_artifact_id,
            found_at,
        )
        .map(|row| row.row_id)
    }
}

#[derive(Clone, Debug)]
pub struct DigRelicRecyclingService<P> {
    port: P,
}

impl<P> DigRelicRecyclingService<P> {
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }
}

impl<P> DigRelicRecyclingService<P>
where
    P: RelicRecyclePort,
{
    pub fn recycle_relics(
        &self,
        discord_id: i64,
        guild_id: i64,
        selected_row_ids: &[i64],
        found_at: i64,
        entropy: &mut impl RelicEntropy,
    ) -> RecycleRelicsOutcome {
        let distinct = selected_row_ids.iter().copied().collect::<BTreeSet<_>>();
        if selected_row_ids.len() != 3 || distinct.len() != 3 {
            return RecycleRelicsOutcome::rejected(
                "Select exactly three different relics to recycle.",
            );
        }

        let owned = match self.port.owned_relic_rows(discord_id, guild_id) {
            Ok(owned) => owned,
            Err(error) => return RecycleRelicsOutcome::rejected(error.to_string()),
        };
        let owned_by_row = owned
            .iter()
            .map(|row| (row.row_id, row))
            .collect::<BTreeMap<_, _>>();
        let selected = selected_row_ids
            .iter()
            .map(|row_id| owned_by_row.get(row_id).copied())
            .collect::<Vec<_>>();
        if selected
            .iter()
            .any(|row| row.is_none_or(|row| !row.is_relic))
        {
            return RecycleRelicsOutcome::rejected(
                "All selected relics must be in your inventory.",
            );
        }
        let selected = selected.into_iter().flatten().collect::<Vec<_>>();
        if selected.iter().any(|row| row.equipped) {
            return RecycleRelicsOutcome::rejected("Only unequipped relics can be recycled.");
        }

        let catalog = artifact_catalog();
        let definitions = selected
            .iter()
            .map(|row| {
                catalog
                    .iter()
                    .find(|artifact| artifact.id == row.artifact_id)
                    .copied()
            })
            .collect::<Vec<_>>();
        if definitions
            .iter()
            .any(|definition| definition.is_none_or(|artifact| !is_ordinary_relic(artifact)))
        {
            return RecycleRelicsOutcome::rejected(
                "Progression and boss-exclusive relics cannot be recycled.",
            );
        }
        let definitions = definitions.into_iter().flatten().collect::<Vec<_>>();
        let rarities = definitions
            .iter()
            .map(|artifact| artifact.rarity)
            .collect::<BTreeSet<_>>();
        if rarities.len() != 1 {
            return RecycleRelicsOutcome::rejected("All three relics must have the same rarity.");
        }
        let source_rarity = *rarities.first().expect("one selected rarity");
        let Some(output_rarity) = next_relic_rarity(source_rarity) else {
            return RecycleRelicsOutcome::rejected("Legendary relics cannot be recycled further.");
        };
        let output_pool = catalog
            .into_iter()
            .filter(|artifact| is_ordinary_relic(*artifact) && artifact.rarity == output_rarity)
            .collect::<Vec<_>>();
        let output = output_pool[entropy.choose_index(output_pool.len())];
        match self
            .port
            .atomic_recycle(discord_id, guild_id, selected_row_ids, output.id, found_at)
        {
            Ok(row_id) => RecycleRelicsOutcome::recycled(source_rarity, output, row_id),
            Err(error) => RecycleRelicsOutcome::rejected(error.to_string()),
        }
    }
}

#[must_use]
pub fn is_ordinary_relic(artifact: ArtifactDef) -> bool {
    dig_prestige4_content::is_ordinary_relic(artifact)
}

#[must_use]
pub fn next_relic_rarity(rarity: Rarity) -> Option<Rarity> {
    RELIC_RARITY_ORDER
        .iter()
        .position(|candidate| *candidate == rarity)
        .and_then(|index| RELIC_RARITY_ORDER.get(index + 1))
        .copied()
}

#[cfg(test)]
#[path = "dig_relic_recycling/tests.rs"]
mod tests;
