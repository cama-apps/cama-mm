use std::convert::Infallible;

use cama_db::dig_inventory_repository::{
    AutoBuyItemOutcome, AutoBuyRequest, BuyInsuranceOutcome, BuyItemOutcome, BuyItemRequest,
    InventoryItemRecord, QueueItemOutcome, SetTrapOutcome,
};

use super::{DigInventoryPort, DigInventoryService};

#[derive(Clone, Debug, Default)]
struct ProjectionPort {
    inventory: Option<Vec<InventoryItemRecord>>,
}

impl DigInventoryPort for ProjectionPort {
    type Error = Infallible;

    fn buy_item_atomic(&self, _request: BuyItemRequest<'_>) -> Result<BuyItemOutcome, Self::Error> {
        panic!("validation should reject before persistence")
    }

    fn ensure_auto_buy_items_atomic(
        &self,
        _request: AutoBuyRequest<'_>,
    ) -> Result<Vec<AutoBuyItemOutcome>, Self::Error> {
        panic!("not used by projection tests")
    }

    fn queue_item_type_atomic(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _item_type: &str,
    ) -> Result<QueueItemOutcome, Self::Error> {
        panic!("validation should reject before persistence")
    }

    fn queue_item_id_atomic(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _item_id: i64,
    ) -> Result<QueueItemOutcome, Self::Error> {
        panic!("not used by projection tests")
    }

    fn inventory_for_tunnel(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
    ) -> Result<Option<Vec<InventoryItemRecord>>, Self::Error> {
        Ok(self.inventory.clone())
    }

    fn set_trap_atomic(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _game_date: &str,
    ) -> Result<SetTrapOutcome, Self::Error> {
        panic!("not used by projection tests")
    }

    fn buy_insurance_atomic(
        &self,
        _discord_id: i64,
        _guild_id: Option<i64>,
        _now: i64,
    ) -> Result<BuyInsuranceOutcome, Self::Error> {
        panic!("not used by projection tests")
    }
}

#[test]
fn test_buy_item_unknown_type_rejected() {
    let service = DigInventoryService::new(ProjectionPort::default());
    let result = service
        .buy_item_at(10_001, Some(12_345), "made_up_item", 100)
        .expect("infallible projection port");
    assert!(!result.success);
    assert_eq!(
        result.error.as_deref(),
        Some("Unknown item type: made_up_item")
    );
}

#[test]
fn test_use_item_unknown_type_rejected() {
    let service = DigInventoryService::new(ProjectionPort::default());
    let result = service
        .use_item(10_001, Some(12_345), "not_a_consumable")
        .expect("infallible projection port");
    assert!(!result.success);
    assert_eq!(
        result.error.as_deref(),
        Some("Unknown item type: not_a_consumable")
    );
}

#[test]
fn test_use_streak_charm_rejected_as_passive() {
    let service = DigInventoryService::new(ProjectionPort::default());
    let result = service
        .use_item(10_001, Some(12_345), "streak_charm")
        .expect("infallible projection port");
    assert!(!result.success);
    assert_eq!(
        result.error.as_deref(),
        Some("Streak Charm is passive and triggers automatically.")
    );
}

#[test]
fn test_get_inventory_empty_without_tunnel() {
    let service = DigInventoryService::new(ProjectionPort::default());
    assert!(
        service
            .get_inventory(10_001, Some(12_345))
            .expect("infallible projection port")
            .is_empty()
    );
}

#[test]
fn test_get_inventory_marks_queued_item() {
    let service = DigInventoryService::new(ProjectionPort {
        inventory: Some(vec![
            InventoryItemRecord {
                id: 1,
                discord_id: 10_001,
                guild_id: 12_345,
                item_type: "dynamite".to_owned(),
                queued: true,
                created_at: 100,
            },
            InventoryItemRecord {
                id: 2,
                discord_id: 10_001,
                guild_id: 12_345,
                item_type: "torch".to_owned(),
                queued: false,
                created_at: 100,
            },
        ]),
    });
    let listing = service
        .get_inventory(10_001, Some(12_345))
        .expect("infallible projection port");
    let dynamite = listing
        .iter()
        .find(|item| item.item_type == "dynamite")
        .expect("dynamite listing");
    let torch = listing
        .iter()
        .find(|item| item.item_type == "torch")
        .expect("torch listing");
    assert!(dynamite.queued);
    assert!(!torch.queued);
    assert_eq!(dynamite.name, "Dynamite");
    assert!(!dynamite.description.is_empty());
}
