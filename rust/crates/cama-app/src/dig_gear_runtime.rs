//! Cohesive application orchestration for live `/dig gear` commands.
//!
//! Every mutation is evaluated by the canonical domain aggregate, then
//! guarded and committed by [`cama_db::dig_gear_runtime`] as one SQLite unit.
//! Discord adapters receive typed panels/outcomes and never own gear SQL.

use std::path::Path;

use cama_db::dig_gear_runtime::{
    DigGearRuntimeCommit, DigGearRuntimeCommitOutcome, DigGearRuntimeRepository,
    DigGearRuntimeRepositoryError, DigGearRuntimeSnapshot, DigGearRuntimeState,
};
use cama_domain::dig_gear::{
    AMULET_TIERS, ARMOR_TIERS, ActionResult, Artifact, BOOTS_TIERS, GearDefinition, GearPiece,
    GearService, GearSlot, GearStore, WEAPON_TIERS,
};
use thiserror::Error;

use crate::dig_inventory::consumable_description;
use crate::dig_loot::CONSUMABLES;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearRuntimePiece {
    pub id: i64,
    pub slot: GearSlot,
    pub tier: u8,
    pub item_id: Option<String>,
    pub name: String,
    pub durability: i32,
    pub max_durability: i32,
    pub equipped: bool,
    pub effect: Option<String>,
}

impl From<&GearPiece> for DigGearRuntimePiece {
    fn from(piece: &GearPiece) -> Self {
        Self {
            id: piece.id,
            slot: piece.slot,
            tier: piece.tier,
            item_id: piece.item_id.clone(),
            name: piece.name().unwrap_or("Unknown Gear").to_owned(),
            durability: piece.durability,
            max_durability: piece.max_durability(),
            equipped: piece.equipped,
            effect: piece.effect_summary().map(str::to_owned),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearRuntimeRelic {
    pub row_id: i64,
    pub artifact_id: String,
    pub equipped: bool,
}

impl From<&Artifact> for DigGearRuntimeRelic {
    fn from(artifact: &Artifact) -> Self {
        Self {
            row_id: artifact.id,
            artifact_id: artifact.artifact_id.clone(),
            equipped: artifact.equipped,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearRuntimePanel {
    pub pieces: Vec<DigGearRuntimePiece>,
    pub relics: Vec<DigGearRuntimeRelic>,
    pub relic_cap: i32,
    pub balance: i64,
    pub trim_notice: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearShopConsumable {
    pub id: String,
    pub name: String,
    pub price: i64,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearShopPickaxe {
    pub tier: u8,
    pub name: String,
    pub price: i64,
    pub depth_required: i64,
    pub prestige_required: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearShopGear {
    pub slot: GearSlot,
    pub tier: u8,
    pub name: String,
    pub price: i64,
    pub depth_required: i64,
    pub prestige_required: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearShop {
    pub consumables: Vec<DigGearShopConsumable>,
    pub pickaxe_upgrades: Vec<DigGearShopPickaxe>,
    pub gear_for_sale: Vec<DigGearShopGear>,
    pub inventory_count: usize,
    pub owned_pickaxe_tier: u8,
    pub balance: i64,
    pub has_tunnel: bool,
}

/// A value/label pair suitable for the `/dig buy` autocomplete boundary.
/// Values deliberately use the same stable identifiers consumed by the buy
/// command: consumable ids and `slot:tier` gear keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearShopChoice {
    pub name: String,
    pub value: String,
}

/// Filter the live shop projection into Discord-safe buy choices.
#[must_use]
pub fn shop_autocomplete(shop: &DigGearShop, query: &str) -> Vec<DigGearShopChoice> {
    let query = query.to_ascii_lowercase();
    let mut choices = Vec::new();
    let mut push = |name: &str, value: String| {
        let name_lower = name.to_ascii_lowercase();
        if query.is_empty() || name_lower.contains(&query) || value.contains(&query) {
            choices.push(DigGearShopChoice {
                name: name.to_owned(),
                value,
            });
        }
    };

    for item in &shop.consumables {
        push(&item.name, item.id.clone());
    }
    for item in &shop.pickaxe_upgrades {
        push(&item.name, format!("weapon:{}", item.tier));
    }
    for item in &shop.gear_for_sale {
        push(&item.name, format!("{}:{}", item.slot.as_str(), item.tier));
    }
    choices.truncate(25);
    choices
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DigGearRuntimeAction {
    EquipGear { gear_id: i64 },
    UnequipGear { gear_id: i64 },
    RepairGear { gear_id: i64 },
    RepairAll,
    BuyPickaxe { tier: i32 },
    BuyGear { slot: String, tier: i32 },
    EquipRelic { artifact_row_id: i64 },
    UnequipRelic { artifact_row_id: i64 },
    PopTrimNotice,
}

impl DigGearRuntimeAction {
    #[must_use]
    pub const fn audit_type(&self) -> &'static str {
        match self {
            Self::EquipGear { .. } => "gear_equip",
            Self::UnequipGear { .. } => "gear_unequip",
            Self::RepairGear { .. } => "gear_repair",
            Self::RepairAll => "gear_repair_all",
            Self::BuyPickaxe { .. } => "pickaxe_buy",
            Self::BuyGear { .. } => "gear_buy",
            Self::EquipRelic { .. } => "relic_equip",
            Self::UnequipRelic { .. } => "relic_unequip",
            Self::PopTrimNotice => "relic_trim_notice",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigGearRuntimeOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub action_id: Option<i64>,
    pub gear_id: Option<i64>,
    pub slot: Option<GearSlot>,
    pub cost: i64,
    pub repaired: usize,
    pub relic_cap: Option<i32>,
    pub balance_after: i64,
    pub panel: Option<DigGearRuntimePanel>,
}

impl DigGearRuntimeOutcome {
    fn rejected(balance: i64, error: impl Into<String>) -> Self {
        Self {
            success: false,
            error: Some(error.into()),
            action_id: None,
            gear_id: None,
            slot: None,
            cost: 0,
            repaired: 0,
            relic_cap: None,
            balance_after: balance,
            panel: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DigGearRuntimeError {
    #[error(transparent)]
    Repository(#[from] DigGearRuntimeRepositoryError),
    #[error("Dig gear aggregate persistence failed")]
    AggregatePersistence,
}

#[derive(Clone, Debug)]
pub struct DigGearRuntimeService {
    repository: DigGearRuntimeRepository,
}

impl DigGearRuntimeService {
    #[must_use]
    pub fn sqlite(database_path: impl AsRef<Path>) -> Self {
        Self {
            repository: DigGearRuntimeRepository::new(database_path),
        }
    }

    pub fn panel(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigGearRuntimePanel>, DigGearRuntimeError> {
        let Some(snapshot) = self.repository.snapshot(discord_id, guild_id)? else {
            return Ok(None);
        };
        Ok(Some(panel_from_snapshot(&snapshot)))
    }

    pub fn shop(
        &self,
        discord_id: i64,
        guild_id: i64,
    ) -> Result<Option<DigGearShop>, DigGearRuntimeError> {
        let Some(snapshot) = self.repository.shop_snapshot(discord_id, guild_id)? else {
            return Ok(None);
        };
        let owned_pickaxe_tier = snapshot
            .equipped_weapon_tier
            .or_else(|| snapshot.tunnel.as_ref().map(|tunnel| tunnel.pickaxe_tier))
            .unwrap_or(0);
        let pickaxe_upgrades = WEAPON_TIERS
            .iter()
            .skip(usize::from(owned_pickaxe_tier).saturating_add(1))
            .map(|definition| DigGearShopPickaxe {
                tier: definition.tier,
                name: pickaxe_display_name(definition).to_owned(),
                price: definition.shop_price,
                depth_required: definition.depth_required,
                prestige_required: definition.prestige_required,
            })
            .collect();
        let gear_for_sale = [
            (GearSlot::Armor, &ARMOR_TIERS),
            (GearSlot::Boots, &BOOTS_TIERS),
            (GearSlot::Amulet, &AMULET_TIERS),
        ]
        .into_iter()
        .flat_map(|(slot, table)| {
            table
                .iter()
                .filter(|definition| definition.shop_price > 0)
                .map(move |definition| DigGearShopGear {
                    slot,
                    tier: definition.tier,
                    name: definition.name.to_owned(),
                    price: definition.shop_price,
                    depth_required: definition.depth_required,
                    prestige_required: definition.prestige_required,
                })
        })
        .collect();
        Ok(Some(DigGearShop {
            consumables: CONSUMABLES
                .iter()
                .map(|item| DigGearShopConsumable {
                    id: item.id.to_owned(),
                    name: item.name.to_owned(),
                    price: item.cost,
                    description: consumable_description(item.id).to_owned(),
                })
                .collect(),
            pickaxe_upgrades,
            gear_for_sale,
            inventory_count: snapshot.inventory_count,
            owned_pickaxe_tier,
            balance: snapshot.balance,
            has_tunnel: snapshot.tunnel.is_some(),
        }))
    }

    pub fn execute(
        &self,
        discord_id: i64,
        guild_id: i64,
        action: DigGearRuntimeAction,
        now: i64,
    ) -> Result<DigGearRuntimeOutcome, DigGearRuntimeError> {
        let Some(snapshot) = self.repository.snapshot(discord_id, guild_id)? else {
            return Ok(DigGearRuntimeOutcome::rejected(
                0,
                "You don't have a tunnel.",
            ));
        };
        let mut service = service_from_snapshot(&snapshot);
        let result = apply_action(&mut service, discord_id, guild_id, &action)?;
        if !result.success {
            return Ok(outcome_from_action(result, snapshot.balance, None, None));
        }
        let proposed = state_from_service(&service, discord_id, guild_id)?;
        let detail = action_detail(&action, &result, &proposed);
        let commit = self.repository.commit(DigGearRuntimeCommit {
            expected: &snapshot,
            proposed: &proposed,
            action_type: action.audit_type(),
            detail_json: &detail,
            now,
        })?;
        match commit {
            DigGearRuntimeCommitOutcome::Applied(receipt) => Ok(outcome_from_action(
                result,
                receipt.balance_after,
                Some(receipt.action_id),
                Some(panel_from_state(&proposed)),
            )),
            DigGearRuntimeCommitOutcome::Conflict => Ok(DigGearRuntimeOutcome::rejected(
                snapshot.balance,
                "Your gear changed before that action committed. Reopen `/dig gear`.",
            )),
            DigGearRuntimeCommitOutcome::MissingPlayer => Ok(DigGearRuntimeOutcome::rejected(
                0,
                "You must be registered first. Use `/player register`.",
            )),
            DigGearRuntimeCommitOutcome::MissingTunnel => Ok(DigGearRuntimeOutcome::rejected(
                snapshot.balance,
                "You don't have a tunnel.",
            )),
        }
    }
}

fn service_from_snapshot(snapshot: &DigGearRuntimeSnapshot) -> GearService {
    GearService {
        store: GearStore::from_persisted_player(
            snapshot.discord_id,
            snapshot.guild_id,
            snapshot.tunnel.clone(),
            snapshot.balance,
            snapshot.gear.clone(),
            snapshot.artifacts.clone(),
            snapshot.next_gear_id,
            snapshot.next_artifact_id,
            snapshot.clock,
        ),
    }
}

fn apply_action(
    service: &mut GearService,
    discord_id: i64,
    guild_id: i64,
    action: &DigGearRuntimeAction,
) -> Result<ActionResult, DigGearRuntimeError> {
    Ok(match action {
        DigGearRuntimeAction::EquipGear { gear_id } => {
            service.equip_gear(discord_id, guild_id, *gear_id)
        }
        DigGearRuntimeAction::UnequipGear { gear_id } => {
            service.unequip_gear(discord_id, guild_id, *gear_id)
        }
        DigGearRuntimeAction::RepairGear { gear_id } => {
            service.repair_gear(discord_id, guild_id, *gear_id)
        }
        DigGearRuntimeAction::RepairAll => service.repair_all_gear(discord_id, guild_id),
        DigGearRuntimeAction::BuyPickaxe { tier } => {
            buy_pickaxe(service, discord_id, guild_id, *tier)?
        }
        DigGearRuntimeAction::BuyGear { slot, tier } if slot == "weapon" => {
            buy_pickaxe(service, discord_id, guild_id, *tier)?
        }
        DigGearRuntimeAction::BuyGear { slot, tier } => service
            .buy_gear(discord_id, guild_id, slot, *tier)
            .map_err(|_| DigGearRuntimeError::AggregatePersistence)?,
        DigGearRuntimeAction::EquipRelic { artifact_row_id } => {
            service.equip_relic(discord_id, guild_id, *artifact_row_id)
        }
        DigGearRuntimeAction::UnequipRelic { artifact_row_id } => {
            service.unequip_relic(discord_id, guild_id, *artifact_row_id)
        }
        DigGearRuntimeAction::PopTrimNotice => {
            let popped = service.pop_relic_trim_notice(discord_id, guild_id);
            ActionResult {
                success: popped,
                error: (!popped).then(|| "There is no relic trim notice.".to_owned()),
                ..ActionResult::default()
            }
        }
    })
}

fn buy_pickaxe(
    service: &mut GearService,
    discord_id: i64,
    guild_id: i64,
    target_tier: i32,
) -> Result<ActionResult, DigGearRuntimeError> {
    let Some(tunnel) = service.store.tunnel(discord_id, guild_id).cloned() else {
        return Ok(ActionResult {
            error: Some("You don't have a tunnel.".to_owned()),
            ..ActionResult::default()
        });
    };
    let owned_tier = service
        .store
        .equipped_gear(discord_id, guild_id)
        .get(&GearSlot::Weapon)
        .map_or(tunnel.pickaxe_tier, |weapon| weapon.tier);
    let target = u8::try_from(target_tier).ok();
    if target != owned_tier.checked_add(1) {
        if target.is_some_and(|tier| tier <= owned_tier) {
            return Ok(ActionResult {
                error: Some("You already have that pickaxe tier or higher.".to_owned()),
                ..ActionResult::default()
            });
        }
        let next_name = owned_tier
            .checked_add(1)
            .and_then(|tier| WEAPON_TIERS.get(usize::from(tier)))
            .map_or("next tier", pickaxe_display_name);
        return Ok(ActionResult {
            error: Some(format!(
                "Buy {next_name} first — pickaxes upgrade one tier at a time."
            )),
            ..ActionResult::default()
        });
    }
    if usize::from(owned_tier) >= WEAPON_TIERS.len().saturating_sub(1) {
        return Ok(ActionResult {
            error: Some("Already at max pickaxe tier.".to_owned()),
            ..ActionResult::default()
        });
    }
    let definition = &WEAPON_TIERS[usize::from(target.expect("checked target tier"))];
    let reached_depth = tunnel.depth.max(tunnel.max_depth);
    if reached_depth < definition.depth_required {
        return Ok(ActionResult {
            error: Some(format!(
                "Need depth {} (you have {reached_depth}).",
                definition.depth_required
            )),
            ..ActionResult::default()
        });
    }
    if tunnel.prestige_level < definition.prestige_required {
        return Ok(ActionResult {
            error: Some(format!(
                "Need prestige level {}.",
                definition.prestige_required
            )),
            ..ActionResult::default()
        });
    }
    let balance = service.store.balance(discord_id, guild_id);
    if balance < definition.shop_price {
        return Ok(ActionResult {
            error: Some(format!(
                "Costs {} JC but you only have {balance} JC.",
                definition.shop_price
            )),
            ..ActionResult::default()
        });
    }
    let Some(gear_id) = service
        .store
        .atomic_purchase_gear(
            discord_id,
            guild_id,
            definition.shop_price,
            GearSlot::Weapon,
            definition.tier,
        )
        .map_err(|_| DigGearRuntimeError::AggregatePersistence)?
    else {
        return Ok(ActionResult {
            error: Some(format!(
                "Costs {} JC but you no longer have it.",
                definition.shop_price
            )),
            ..ActionResult::default()
        });
    };
    service
        .store
        .equip_gear(gear_id, discord_id, guild_id, GearSlot::Weapon)
        .map_err(|_| DigGearRuntimeError::AggregatePersistence)?;
    service
        .store
        .tunnel_mut(discord_id, guild_id)
        .ok_or(DigGearRuntimeError::AggregatePersistence)?
        .pickaxe_tier = definition.tier;
    Ok(ActionResult {
        success: true,
        gear_id: Some(gear_id),
        slot: Some(GearSlot::Weapon),
        cost: definition.shop_price,
        ..ActionResult::default()
    })
}

fn pickaxe_display_name(definition: &GearDefinition) -> &str {
    definition
        .name
        .strip_suffix(" Pickaxe")
        .unwrap_or(definition.name)
}

fn state_from_service(
    service: &GearService,
    discord_id: i64,
    guild_id: i64,
) -> Result<DigGearRuntimeState, DigGearRuntimeError> {
    let tunnel = service
        .store
        .tunnel(discord_id, guild_id)
        .cloned()
        .ok_or(DigGearRuntimeError::AggregatePersistence)?;
    Ok(DigGearRuntimeState {
        tunnel,
        balance: service.store.balance(discord_id, guild_id),
        gear: service.store.persisted_gear(discord_id, guild_id),
        artifacts: service.store.persisted_artifacts(discord_id, guild_id),
    })
}

fn panel_from_snapshot(snapshot: &DigGearRuntimeSnapshot) -> DigGearRuntimePanel {
    panel(
        &snapshot.gear,
        &snapshot.artifacts,
        snapshot.tunnel.prestige_level,
        snapshot.balance,
        snapshot.tunnel.relic_trim_notice,
    )
}

fn panel_from_state(state: &DigGearRuntimeState) -> DigGearRuntimePanel {
    panel(
        &state.gear,
        &state.artifacts,
        state.tunnel.prestige_level,
        state.balance,
        state.tunnel.relic_trim_notice,
    )
}

fn panel(
    gear: &[GearPiece],
    artifacts: &[Artifact],
    prestige: i32,
    balance: i64,
    trim_notice: bool,
) -> DigGearRuntimePanel {
    let mut pieces = gear
        .iter()
        .map(DigGearRuntimePiece::from)
        .collect::<Vec<_>>();
    pieces.sort_by(|left, right| {
        left.slot
            .as_str()
            .cmp(right.slot.as_str())
            .then_with(|| right.tier.cmp(&left.tier))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut relics = artifacts
        .iter()
        .filter(|artifact| artifact.is_relic)
        .map(DigGearRuntimeRelic::from)
        .collect::<Vec<_>>();
    relics.sort_by_key(|artifact| artifact.row_id);
    DigGearRuntimePanel {
        pieces,
        relics,
        relic_cap: GearService::relic_slot_cap(prestige),
        balance,
        trim_notice,
    }
}

fn outcome_from_action(
    result: ActionResult,
    balance_after: i64,
    action_id: Option<i64>,
    panel: Option<DigGearRuntimePanel>,
) -> DigGearRuntimeOutcome {
    DigGearRuntimeOutcome {
        success: result.success,
        error: result.error,
        action_id,
        gear_id: result.gear_id,
        slot: result.slot,
        cost: result.cost,
        repaired: result.repaired,
        relic_cap: result.cap,
        balance_after,
        panel,
    }
}

fn action_detail(
    action: &DigGearRuntimeAction,
    result: &ActionResult,
    proposed: &DigGearRuntimeState,
) -> String {
    serde_json::json!({
        "action": action.audit_type(),
        "gear_id": result.gear_id,
        "slot": result.slot.map(GearSlot::as_str),
        "cost": result.cost,
        "repaired": result.repaired,
        "relic_cap": result.cap,
        "balance_after": proposed.balance,
    })
    .to_string()
}

#[cfg(test)]
#[path = "dig_gear_runtime/tests.rs"]
mod tests;
