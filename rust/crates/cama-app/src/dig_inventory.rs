//! Transport-independent dig inventory and defense orchestration.
//!
//! Validation, catalog lookup, result shaping, and status ordering live here
//! behind a typed persistence port. [`DigInventoryRepository`] implements that
//! port against Python's existing SQLite schema; Discord command registration,
//! process startup, and game-date acquisition remain outside this additive
//! module while Python is the live runtime.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use cama_db::dig_inventory_repository::{
    AutoBuyItemOutcome, AutoBuyRequest, AutoBuySelection, AutoBuyStatus, BuyInsuranceOutcome,
    BuyItemOutcome, BuyItemRequest, DigInventoryRepository, DigInventoryRepositoryError,
    InventoryItemRecord, QueueItemOutcome, SetTrapOutcome,
};

use crate::dig_loot::{MAX_INVENTORY_SLOTS, consumable};

const AUTO_QUEUE_ON_BUY: [&str; 2] = ["hard_hat", "torch"];

/// Persistence boundary for inventory and tunnel-defense actions.
///
/// Every method that can move JC or change a queue is a single atomic port
/// call. An adapter must not implement these methods as read-then-write pairs.
pub trait DigInventoryPort {
    type Error;

    fn buy_item_atomic(&self, request: BuyItemRequest<'_>) -> Result<BuyItemOutcome, Self::Error>;

    fn ensure_auto_buy_items_atomic(
        &self,
        request: AutoBuyRequest<'_>,
    ) -> Result<Vec<AutoBuyItemOutcome>, Self::Error>;

    fn queue_item_type_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
    ) -> Result<QueueItemOutcome, Self::Error>;

    fn queue_item_id_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: i64,
    ) -> Result<QueueItemOutcome, Self::Error>;

    fn inventory_for_tunnel(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Vec<InventoryItemRecord>>, Self::Error>;

    fn set_trap_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        game_date: &str,
    ) -> Result<SetTrapOutcome, Self::Error>;

    fn buy_insurance_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<BuyInsuranceOutcome, Self::Error>;
}

impl DigInventoryPort for DigInventoryRepository {
    type Error = DigInventoryRepositoryError;

    fn buy_item_atomic(&self, request: BuyItemRequest<'_>) -> Result<BuyItemOutcome, Self::Error> {
        DigInventoryRepository::buy_item_atomic(self, request)
    }

    fn ensure_auto_buy_items_atomic(
        &self,
        request: AutoBuyRequest<'_>,
    ) -> Result<Vec<AutoBuyItemOutcome>, Self::Error> {
        DigInventoryRepository::ensure_auto_buy_items_atomic(self, request)
    }

    fn queue_item_type_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
    ) -> Result<QueueItemOutcome, Self::Error> {
        DigInventoryRepository::queue_item_type_atomic(self, discord_id, guild_id, item_type)
    }

    fn queue_item_id_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: i64,
    ) -> Result<QueueItemOutcome, Self::Error> {
        DigInventoryRepository::queue_item_id_atomic(self, discord_id, guild_id, item_id)
    }

    fn inventory_for_tunnel(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<Vec<InventoryItemRecord>>, Self::Error> {
        DigInventoryRepository::inventory_for_tunnel(self, discord_id, guild_id)
    }

    fn set_trap_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        game_date: &str,
    ) -> Result<SetTrapOutcome, Self::Error> {
        DigInventoryRepository::set_trap_atomic(self, discord_id, guild_id, game_date)
    }

    fn buy_insurance_atomic(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<BuyInsuranceOutcome, Self::Error> {
        DigInventoryRepository::buy_insurance_atomic(self, discord_id, guild_id, now)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DigInventoryAction {
    pub success: bool,
    pub error: Option<String>,
    pub item: Option<String>,
    pub item_id: Option<i64>,
    pub queued: bool,
    pub cost: i64,
    pub balance_after: Option<i64>,
    pub expires_at: Option<i64>,
    pub message: Option<String>,
}

impl DigInventoryAction {
    fn error(message: impl Into<String>) -> Self {
        Self {
            error: Some(message.into()),
            ..Self::default()
        }
    }

    fn success() -> Self {
        Self {
            success: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoBuyAction {
    pub item_type: String,
    pub item: String,
    pub status: AutoBuyStatus,
    pub cost: i64,
    pub item_id: Option<i64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryViewItem {
    pub item_type: String,
    pub name: String,
    pub description: String,
    pub queued: bool,
}

pub struct DigInventoryService<P> {
    port: P,
}

impl<P> DigInventoryService<P>
where
    P: DigInventoryPort,
{
    #[must_use]
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    #[must_use]
    pub const fn port(&self) -> &P {
        &self.port
    }

    pub fn buy_item(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
    ) -> Result<DigInventoryAction, P::Error> {
        self.buy_item_at(discord_id, guild_id, item_type, unix_timestamp())
    }

    pub fn buy_item_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
        created_at: i64,
    ) -> Result<DigInventoryAction, P::Error> {
        let Some(definition) = consumable(item_type) else {
            return Ok(DigInventoryAction::error(format!(
                "Unknown item type: {item_type}"
            )));
        };
        let outcome = self.port.buy_item_atomic(BuyItemRequest {
            discord_id,
            guild_id,
            item_type: definition.id,
            price: definition.cost,
            inventory_limit: MAX_INVENTORY_SLOTS,
            auto_queue_if_available: AUTO_QUEUE_ON_BUY.contains(&definition.id),
            created_at,
        })?;
        Ok(match outcome {
            BuyItemOutcome::Purchased {
                item_id,
                queued,
                cost,
                balance_after,
            } => DigInventoryAction {
                item: Some(definition.name.to_owned()),
                item_id: Some(item_id),
                queued,
                cost,
                balance_after: Some(balance_after),
                ..DigInventoryAction::success()
            },
            BuyItemOutcome::MissingTunnel => {
                DigInventoryAction::error("You don't have a tunnel. Dig first!")
            }
            BuyItemOutcome::InventoryFull { limit } => {
                DigInventoryAction::error(format!("Inventory full ({limit} items max)."))
            }
            BuyItemOutcome::InsufficientBalance { balance, cost } => DigInventoryAction::error(
                format!("Costs {cost} JC but you only have {balance} JC."),
            ),
        })
    }

    pub fn ensure_auto_buy_items(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_types: &[&str],
        reserved_balance: i64,
    ) -> Result<Vec<AutoBuyAction>, P::Error> {
        self.ensure_auto_buy_items_at(
            discord_id,
            guild_id,
            item_types,
            reserved_balance,
            unix_timestamp(),
            None,
        )
    }

    /// Deterministic seam for scheduled dig orchestration and stale-snapshot
    /// parity tests. Production callers normally pass `None` for the observed
    /// balance and let the repository use its live transaction snapshot.
    pub fn ensure_auto_buy_items_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_types: &[&str],
        reserved_balance: i64,
        created_at: i64,
        observed_balance: Option<i64>,
    ) -> Result<Vec<AutoBuyAction>, P::Error> {
        let selected = item_types
            .iter()
            .filter_map(|item_type| {
                if !AUTO_QUEUE_ON_BUY.contains(item_type) {
                    return None;
                }
                let definition = consumable(item_type)?;
                Some(AutoBuySelection {
                    item_type: definition.id,
                    price: definition.cost,
                })
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let outcomes = self.port.ensure_auto_buy_items_atomic(AutoBuyRequest {
            discord_id,
            guild_id,
            selections: &selected,
            reserved_balance,
            inventory_limit: MAX_INVENTORY_SLOTS,
            created_at,
            observed_balance,
        })?;
        Ok(outcomes.into_iter().map(auto_buy_action).collect())
    }

    pub fn use_item(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_type: &str,
    ) -> Result<DigInventoryAction, P::Error> {
        let Some(definition) = consumable(item_type) else {
            return Ok(DigInventoryAction::error(format!(
                "Unknown item type: {item_type}"
            )));
        };
        if definition.id == "streak_charm" {
            return Ok(DigInventoryAction::error(
                "Streak Charm is passive and triggers automatically.",
            ));
        }
        let outcome = self
            .port
            .queue_item_type_atomic(discord_id, guild_id, definition.id)?;
        Ok(queue_outcome_action(outcome, Some(definition.name)))
    }

    pub fn queue_item(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: i64,
    ) -> Result<DigInventoryAction, P::Error> {
        let outcome = self
            .port
            .queue_item_id_atomic(discord_id, guild_id, item_id)?;
        Ok(queue_outcome_action(outcome, None))
    }

    pub fn get_inventory(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Vec<InventoryViewItem>, P::Error> {
        let Some(items) = self.port.inventory_for_tunnel(discord_id, guild_id)? else {
            return Ok(Vec::new());
        };
        let queued_types = items
            .iter()
            .filter(|item| item.queued)
            .map(|item| item.item_type.as_str())
            .collect::<HashSet<_>>();
        Ok(items
            .iter()
            .map(|item| {
                let definition = consumable(&item.item_type);
                InventoryViewItem {
                    item_type: item.item_type.clone(),
                    name: definition.map_or_else(
                        || item.item_type.clone(),
                        |definition| definition.name.to_owned(),
                    ),
                    description: consumable_description(&item.item_type).to_owned(),
                    queued: queued_types.contains(item.item_type.as_str()),
                }
            })
            .collect())
    }

    pub fn set_trap(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        game_date: &str,
    ) -> Result<DigInventoryAction, P::Error> {
        let outcome = self.port.set_trap_atomic(discord_id, guild_id, game_date)?;
        Ok(match outcome {
            SetTrapOutcome::Set {
                cost,
                balance_after,
            } => DigInventoryAction {
                cost,
                balance_after: Some(balance_after),
                message: Some("Trap set!".to_owned()),
                ..DigInventoryAction::success()
            },
            SetTrapOutcome::MissingTunnel => DigInventoryAction::error("You don't have a tunnel."),
            SetTrapOutcome::AlreadyActive => {
                DigInventoryAction::error("You already have an active trap.")
            }
            SetTrapOutcome::InsufficientBalance { balance, cost } => DigInventoryAction::error(
                format!("Trap costs {cost} JC but you only have {balance} JC."),
            ),
        })
    }

    pub fn buy_insurance(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> Result<DigInventoryAction, P::Error> {
        self.buy_insurance_at(discord_id, guild_id, unix_timestamp())
    }

    pub fn buy_insurance_at(
        &self,
        discord_id: i64,
        guild_id: Option<i64>,
        now: i64,
    ) -> Result<DigInventoryAction, P::Error> {
        let outcome = self.port.buy_insurance_atomic(discord_id, guild_id, now)?;
        Ok(match outcome {
            BuyInsuranceOutcome::Purchased {
                cost,
                expires_at,
                balance_after,
            } => DigInventoryAction {
                cost,
                expires_at: Some(expires_at),
                balance_after: Some(balance_after),
                ..DigInventoryAction::success()
            },
            BuyInsuranceOutcome::MissingTunnel => {
                DigInventoryAction::error("You don't have a tunnel.")
            }
            BuyInsuranceOutcome::InsufficientBalance { balance, cost } => {
                DigInventoryAction::error(format!(
                    "Insurance costs {cost} JC but you only have {balance} JC."
                ))
            }
        })
    }
}

fn auto_buy_action(outcome: AutoBuyItemOutcome) -> AutoBuyAction {
    let item = consumable(&outcome.item_type)
        .map_or(outcome.item_type.as_str(), |definition| definition.name)
        .to_owned();
    let error = (outcome.status == AutoBuyStatus::SkippedMissingTunnel)
        .then(|| "You don't have a tunnel. Dig first!".to_owned());
    AutoBuyAction {
        item_type: outcome.item_type,
        item,
        status: outcome.status,
        cost: outcome.cost,
        item_id: outcome.item_id,
        error,
    }
}

fn queue_outcome_action(outcome: QueueItemOutcome, known_name: Option<&str>) -> DigInventoryAction {
    match outcome {
        QueueItemOutcome::Queued { .. } => DigInventoryAction {
            // `use_item` returns its display name; direct queue-by-id mirrors
            // Python's smaller `{queued: true}` result.
            item: known_name.map(str::to_owned),
            queued: true,
            ..DigInventoryAction::success()
        },
        QueueItemOutcome::MissingTunnel => DigInventoryAction::error("You don't have a tunnel."),
        QueueItemOutcome::NotOwned => match known_name {
            Some(name) => DigInventoryAction::error(format!("You don't have a {name}.")),
            None => DigInventoryAction::error("That item is not in your inventory."),
        },
        QueueItemOutcome::AlreadyQueued { item_type } => {
            let name = known_name
                .map(str::to_owned)
                .or_else(|| consumable(&item_type).map(|item| item.name.to_owned()))
                .unwrap_or_else(|| "That item".to_owned());
            DigInventoryAction::error(format!("{name} is already queued."))
        }
        QueueItemOutcome::BossPreparationAlreadyQueued => {
            DigInventoryAction::error("Only one boss preparation item can be queued at a time.")
        }
    }
}

#[must_use]
pub const fn consumable_description(item_type: &str) -> &'static str {
    match item_type.as_bytes() {
        b"dynamite" => "+5 bonus blocks on next dig.",
        b"hard_hat" => "Prevents cave-ins entirely for the next 3 digs. Costs luminosity per dig.",
        b"lantern" => "-50% cave-in next dig. Reveals what's stirring nearby.",
        b"reinforcement" => "48h: half damage from sabotage, big cave-ins are capped.",
        b"torch" => "+50 luminosity. Light the way.",
        b"grappling_hook" => "Cushions the next 5 cave-ins: no block loss, no stun.",
        b"sonar_pulse" => "Reveals the next event and lets it pass you by once.",
        b"depth_charge" => "+10 bonus blocks on next dig. A louder, deeper blast than Dynamite.",
        b"void_bait" => "3 digs: 2x event chance, with a thumb on the scale for rare finds.",
        b"streak_charm" => "Passively saves one daily dig streak after exactly one missed day.",
        b"tempered_whetstone" => "One boss attempt: +1 damage on every player hit.",
        b"warding_salts" => "One boss attempt: blocks the first mechanic or status effect.",
        b"rescue_line" => "One boss attempt: halves defeat knockback and extra gear wear.",
        _ => "",
    }
}

fn unix_timestamp() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    i64::try_from(seconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "dig_inventory/tests.rs"]
mod tests;
