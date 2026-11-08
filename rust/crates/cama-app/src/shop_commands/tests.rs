use std::sync::{Arc, Barrier};
use std::thread;

use cama_domain::economy_scaling::DailyRewardPolicy;

use super::*;

fn ping_purchase(
    success: bool,
    reason: Option<PurchaseFailureReason>,
    balance: i64,
    cooldown_ends_at: Option<i64>,
) -> PaidPingPurchase {
    PaidPingPurchase {
        success,
        reason,
        balance,
        cooldown_ends_at,
    }
}

fn package_success(cost: i64, active: i64, added: i64) -> PackageDealSuccess {
    PackageDealSuccess {
        cost,
        active_deal_count: active,
        games_added: added,
        games_remaining: 10,
    }
}

struct ContractPort;

impl AtomicShopSettlementPort for ContractPort {
    type Error = ();

    fn settle_double_or_nothing_atomic(
        &self,
        request: DoubleOrNothingRequest,
    ) -> Result<DoubleOrNothingSettlement, Self::Error> {
        Ok(rejected_double(
            PurchaseFailureReason::OnCooldown,
            0,
            request.won,
            None,
        ))
    }

    fn try_purchase_item_atomic(
        &self,
        request: ManaPurchaseRequest,
    ) -> Result<ManaPurchaseResult, Self::Error> {
        Ok(ManaPurchaseResult {
            success: true,
            reason: None,
            balance: 0,
            purchase_id: Some("purchase-1".to_owned()),
            item_key: request.item_key.to_owned(),
        })
    }

    fn refund_item_purchase_atomic(&self, _purchase_id: &str) -> Result<bool, Self::Error> {
        Ok(true)
    }

    fn get_item_purchase(
        &self,
        _purchase_id: &str,
    ) -> Result<Option<ManaPurchaseResult>, Self::Error> {
        Ok(None)
    }

    fn mark_item_purchase_applying_atomic(&self, _purchase_id: &str) -> Result<bool, Self::Error> {
        Ok(true)
    }

    fn complete_item_purchase_atomic(&self, _purchase_id: &str) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[test]
fn test_repository_contracts_require_atomic_shop_settlement() {
    fn require_contract<T: AtomicShopSettlementPort<Error = ()>>(_port: &T) {}
    let port = ContractPort;
    require_contract(&port);
    assert!(port.refund_item_purchase_atomic("purchase-1").unwrap());
    assert!(
        port.mark_item_purchase_applying_atomic("purchase-1")
            .unwrap()
    );
    assert!(port.complete_item_purchase_atomic("purchase-1").unwrap());
    assert_eq!(port.get_item_purchase("purchase-1").unwrap(), None);
}

#[test]
fn test_pingedash_description_uses_configured_cost() {
    assert_eq!(
        PaidPingKind::Dash.description(),
        format!("Spend {PINGEDASH_COST} jopacoin to send the Pingedash")
    );
}

#[test]
fn test_pingedkevin_description_and_url() {
    assert_eq!(
        PaidPingKind::Kevin.description(),
        format!("Spend {PINGEDKEVIN_COST} jopacoin to send the PingedKevin")
    );
    assert_eq!(
        PaidPingKind::Kevin.tenor_url(),
        "https://tenor.com/view/in-trouble-mad-face-gif-10963449859613356314"
    );
}

#[test]
fn test_pingedash_is_not_a_shop_item() {
    let values: Vec<_> = item_autocomplete("", 10, false)
        .into_iter()
        .map(|choice| choice.value)
        .collect();
    assert!(!values.contains(&"pingedash"));
    assert!(!values.contains(&"pingedkevin"));
}

#[test]
fn test_protect_hero_is_not_a_shop_item() {
    assert!(
        item_autocomplete("", 10, false)
            .iter()
            .all(|choice| choice.value != "protect_hero")
    );
}

#[test]
fn test_shop_buy_has_no_protect_hero_parameter() {
    assert!(!SHOP_BUY_PARAMETER_NAMES.contains(&"hero"));
}

#[test]
fn test_shop_pricing_autocomplete_explains_soft_avoid_fallback_and_range() {
    let choices = item_autocomplete("", 10, false);
    let package = choices
        .iter()
        .find(|choice| choice.value == "package_deal")
        .unwrap();
    let avoid = choices
        .iter()
        .find(|choice| choice.value == "soft_avoid")
        .unwrap();
    assert!(!package.name.contains("FREE"));
    assert!(package.name.contains("0 active"));
    assert_eq!(
        avoid.name,
        "Soft Avoid (500 before 3 shared games; 250-1000 by win rate; lasts 10 games)"
    );
}

#[test]
fn test_package_deal_autocomplete_clamps_configured_duration_to_ten() {
    let package = item_autocomplete("", 25, false)
        .into_iter()
        .find(|choice| choice.value == "package_deal")
        .unwrap();
    assert!(package.name.contains("for 10 games"));
    assert!(!package.name.contains("25 games"));
}

#[test]
fn test_soft_avoid_floor_applies_to_configured_default() {
    assert_eq!(calculate_soft_avoid_cost(None, 100), 250);
    assert_eq!(
        calculate_soft_avoid_cost(
            Some(PairingRecord {
                games_together: 2,
                wins_together: 0,
            }),
            100
        ),
        250
    );
}

#[test]
fn test_pingedash_requires_configured_target() {
    let response = paid_ping_response(PaidPingKind::Dash, None, None);
    assert_eq!(response.visibility, Visibility::Ephemeral);
    assert!(response.content.contains("not configured"));
}

#[test]
fn test_pingedash_success_charges_and_mentions_only_configured_target() {
    let target = 123_456_789_012_345_678;
    let purchase = ping_purchase(true, None, 40, Some(1_700_086_400));
    let response = paid_ping_response(PaidPingKind::Dash, Some(target), Some(&purchase));
    assert_eq!(response.phase, ResponsePhase::FollowupAfterPublicDefer);
    assert_eq!(
        response.content,
        format!("<@{target}>\n{PINGEDASH_TENOR_URL}")
    );
    assert_eq!(response.allowed_users, vec![target]);
}

#[test]
fn test_pingedash_cooldown_response_is_private() {
    let ends_at = 1_700_086_400;
    let purchase = ping_purchase(
        false,
        Some(PurchaseFailureReason::OnCooldown),
        50,
        Some(ends_at),
    );
    let response = paid_ping_response(PaidPingKind::Dash, Some(123), Some(&purchase));
    assert_eq!(response.phase, ResponsePhase::Initial);
    assert_eq!(response.visibility, Visibility::Ephemeral);
    assert!(response.content.contains(&format!("<t:{ends_at}:R>")));
    assert!(!response.content.contains(PINGEDASH_TENOR_URL));
}

#[test]
fn test_pingedash_failure_notice_tolerates_lapsed_interaction() {
    let purchase = ping_purchase(
        false,
        Some(PurchaseFailureReason::InsufficientBalance),
        5,
        None,
    );
    let response = paid_ping_response(PaidPingKind::Dash, Some(123), Some(&purchase));
    assert_eq!(response.phase, ResponsePhase::Initial);
    assert_eq!(response.visibility, Visibility::Ephemeral);
}

#[test]
fn test_pingedkevin_requires_configured_target() {
    let response = paid_ping_response(PaidPingKind::Kevin, None, None);
    assert!(response.content.contains("not configured"));
    assert_eq!(response.visibility, Visibility::Ephemeral);
}

#[test]
fn test_pingedkevin_success_charges_and_mentions_only_configured_target() {
    let target = 234_567_890_123_456_789;
    let purchase = ping_purchase(true, None, 40, Some(1_700_086_400));
    let response = paid_ping_response(PaidPingKind::Kevin, Some(target), Some(&purchase));
    assert_eq!(
        response.content,
        format!("<@{target}>\n{PINGEDKEVIN_TENOR_URL}")
    );
    assert_eq!(response.allowed_users, vec![target]);
    assert_eq!(response.visibility, Visibility::Public);
}

#[test]
fn test_pingedkevin_cooldown_response_is_private() {
    let ends_at = 1_700_086_400;
    let purchase = ping_purchase(
        false,
        Some(PurchaseFailureReason::OnCooldown),
        50,
        Some(ends_at),
    );
    let response = paid_ping_response(PaidPingKind::Kevin, Some(234), Some(&purchase));
    assert_eq!(response.visibility, Visibility::Ephemeral);
    assert!(response.content.contains(&format!("<t:{ends_at}:R>")));
    assert!(!response.content.contains(PINGEDKEVIN_TENOR_URL));
}

#[test]
fn test_double_or_nothing_settlement_rolls_back_and_retries_once() {
    let store = InMemoryShopSettlement::new();
    store.register(1001, 9000, 200);
    let request = DoubleOrNothingRequest {
        account_id: 1001,
        guild_id: 9000,
        cost: 50,
        won: false,
        now: 1_700_000_000,
        cooldown_seconds: 86_400,
        bypass_cooldown: false,
    };
    assert_eq!(
        store.settle_double_or_nothing(request, false),
        Err("forced log failure")
    );
    assert_eq!(store.balance(1001, 9000), Some(200));
    assert!(store.spins().is_empty());
    let settled = store.settle_double_or_nothing(request, true).unwrap();
    let repeated = store
        .settle_double_or_nothing(
            DoubleOrNothingRequest {
                now: request.now + 1,
                won: true,
                ..request
            },
            true,
        )
        .unwrap();
    assert_eq!(settled.balance_at_risk, 150);
    assert_eq!(settled.balance_after, 0);
    assert_eq!(repeated.reason, Some(PurchaseFailureReason::OnCooldown));
    assert_eq!(store.spins().len(), 1);
}

#[test]
fn test_concurrent_double_or_nothing_settlements_store_one_outcome() {
    let store = Arc::new(InMemoryShopSettlement::new());
    store.register(1001, 9000, 200);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = [true, false]
        .into_iter()
        .map(|won| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store
                    .settle_double_or_nothing(
                        DoubleOrNothingRequest {
                            account_id: 1001,
                            guild_id: 9000,
                            cost: 50,
                            won,
                            now: 1_700_000_000,
                            cooldown_seconds: 86_400,
                            bypass_cooldown: false,
                        },
                        true,
                    )
                    .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.success).count(), 1);
    assert_eq!(store.spins().len(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| !result.success)
            .map(|result| result.reason)
            .collect::<Vec<_>>(),
        vec![Some(PurchaseFailureReason::OnCooldown)]
    );
}

#[test]
fn test_handle_announce_requires_registration() {
    let outcome = simple_purchase(SimpleProduct::Announce, 1001, false, 0, false, true);
    assert!(outcome.response.content.contains("`/player register`"));
    assert!(outcome.spend.is_none());
}

#[test]
fn test_handle_announce_insufficient_balance() {
    let outcome = simple_purchase(
        SimpleProduct::Announce,
        1001,
        true,
        SHOP_ANNOUNCE_COST - 1,
        false,
        true,
    );
    assert!(outcome.response.content.contains("You need"));
    assert!(outcome.response.content.contains("only have"));
}

#[test]
fn test_handle_announce_success_deducts_balance() {
    let outcome = simple_purchase(
        SimpleProduct::AnnounceTarget,
        1001,
        true,
        SHOP_ANNOUNCE_TARGET_COST + 10,
        true,
        true,
    );
    assert_eq!(
        outcome.spend,
        Some(SpendIntent {
            amount: SHOP_ANNOUNCE_TARGET_COST,
            source: "shop_announce",
            actor_id: 1001,
        })
    );
    assert!(outcome.delivered);
    assert_eq!(
        outcome.response.embed_title.as_deref(),
        Some("WEALTH ANNOUNCEMENT")
    );
}

#[test]
fn test_handle_jopa_coin_requires_registration() {
    let outcome = simple_purchase(SimpleProduct::JopaCoin, 1001, false, 0, false, true);
    assert!(outcome.response.content.contains("`/player register`"));
    assert!(outcome.spend.is_none());
}

#[test]
fn test_handle_jopa_coin_insufficient_balance() {
    let outcome = simple_purchase(
        SimpleProduct::JopaCoin,
        1001,
        true,
        SHOP_JOPA_COIN_COST - 1,
        false,
        true,
    );
    assert!(outcome.response.content.contains("You need"));
    assert!(outcome.response.content.contains("only have"));
}

#[test]
fn test_handle_jopa_coin_success_deducts_balance() {
    let outcome = simple_purchase(
        SimpleProduct::JopaCoin,
        1001,
        true,
        SHOP_JOPA_COIN_COST + 100,
        true,
        true,
    );
    assert_eq!(outcome.spend.unwrap().source, "shop_jopa_coin");
    assert_eq!(
        outcome.response.phase,
        ResponsePhase::FollowupAfterPublicDefer
    );
    assert!(outcome.response.embed_title.unwrap().contains("Jopa Coin"));
}

#[test]
fn test_handle_jopa_coin_aborts_when_balance_vanishes_before_the_debit() {
    let outcome = simple_purchase(
        SimpleProduct::JopaCoin,
        1001,
        true,
        SHOP_JOPA_COIN_COST + 100,
        false,
        true,
    );
    assert!(!outcome.delivered);
    assert_eq!(outcome.response.phase, ResponsePhase::Initial);
    assert_eq!(outcome.response.visibility, Visibility::Ephemeral);
    assert!(outcome.response.content.contains("no longer have"));
}

#[test]
fn test_handle_jopa_coin_shortfall_tolerates_lapsed_interaction() {
    let outcome = simple_purchase(
        SimpleProduct::JopaCoin,
        1001,
        true,
        SHOP_JOPA_COIN_COST + 100,
        false,
        false,
    );
    assert!(outcome.notice_transport_lapsed);
    assert!(!outcome.delivered);
    assert_eq!(outcome.response.phase, ResponsePhase::Initial);
}

#[test]
fn test_handle_soft_avoid_prices_from_teammate_winrate() {
    for (games_together, wins_together, expected_cost) in [(4, 2, 500), (3, 3, 1_000), (7, 2, 286)]
    {
        let pairing = PairingRecord {
            games_together,
            wins_together,
        };
        let outcome = soft_avoid_command_outcome("Target", Some(pairing), None, Ok(10));
        assert_eq!(outcome.cost, expected_cost);
        assert!(outcome.charged_atomically);
    }
}

#[test]
fn test_handle_soft_avoid_uses_default_price_before_three_games() {
    let outcome = soft_avoid_command_outcome(
        "Target",
        Some(PairingRecord {
            games_together: 2,
            wins_together: 2,
        }),
        None,
        Ok(10),
    );
    assert_eq!(outcome.cost, 500);
}

#[test]
fn test_handle_soft_avoid_uses_default_price_with_no_pairing_data() {
    let outcome = soft_avoid_command_outcome("Target", None, None, Ok(10));
    assert_eq!(outcome.cost, 500);
    assert!(outcome.charged_atomically);
}

#[test]
fn test_handle_soft_avoid_purchase_failure_does_not_charge() {
    let outcome = soft_avoid_command_outcome("Target", None, None, Err(SoftAvoidFailure::Storage));
    assert!(!outcome.charged_atomically);
    assert!(
        outcome
            .response
            .content
            .to_lowercase()
            .contains("not charged")
    );
}

#[test]
fn test_handle_soft_avoid_minimum_price_purchase_failure_does_not_charge() {
    let outcome = soft_avoid_command_outcome(
        "Target",
        Some(PairingRecord {
            games_together: 3,
            wins_together: 0,
        }),
        None,
        Err(SoftAvoidFailure::Storage),
    );
    assert_eq!(outcome.cost, SOFT_AVOID_MIN_COST);
    assert!(!outcome.charged_atomically);
}

#[test]
fn test_handle_soft_avoid_charges_minimum_price_after_three_losses() {
    let outcome = soft_avoid_command_outcome(
        "Target",
        Some(PairingRecord {
            games_together: 3,
            wins_together: 0,
        }),
        None,
        Ok(10),
    );
    assert_eq!(outcome.cost, 250);
    assert!(outcome.charged_atomically);
    assert_eq!(
        outcome.response.phase,
        ResponsePhase::FollowupAfterEphemeralDefer
    );
}

#[test]
fn test_handle_soft_avoid_rejects_active_duplicate_without_charge() {
    let outcome = soft_avoid_command_outcome("Target", None, Some(6), Ok(10));
    assert!(!outcome.charged_atomically);
    assert_eq!(outcome.response.phase, ResponsePhase::Initial);
    assert!(
        outcome
            .response
            .content
            .to_lowercase()
            .contains("already active")
    );
    assert!(outcome.response.content.contains("6 games remaining"));
}

#[test]
fn test_handle_soft_avoid_handles_concurrent_duplicate_without_charge() {
    let outcome = soft_avoid_command_outcome(
        "Target",
        None,
        None,
        Err(SoftAvoidFailure::AlreadyActive { games_remaining: 6 }),
    );
    assert!(!outcome.charged_atomically);
    assert!(outcome.response.content.contains("6 games remaining"));
    assert_eq!(
        outcome.response.phase,
        ResponsePhase::FollowupAfterEphemeralDefer
    );
}

#[test]
fn test_handle_soft_avoid_handles_atomic_insufficient_balance() {
    let outcome = soft_avoid_command_outcome(
        "Target",
        Some(PairingRecord {
            games_together: 3,
            wins_together: 0,
        }),
        None,
        Err(SoftAvoidFailure::InsufficientBalance { balance: 200 }),
    );
    assert!(!outcome.charged_atomically);
    assert!(outcome.response.content.contains("need 250"));
    assert!(outcome.response.content.contains("only have 200"));
}

#[test]
fn test_handle_package_deal_costs_one_jopacoin_with_no_active_deals() {
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        10,
        Ok(package_success(1, 0, 10)),
    );
    assert_eq!(outcome.active_cost, 800);
    assert!(outcome.charged_atomically);
    assert!(outcome.response.content.contains("**Cost:** 1"));
}

#[test]
fn test_handle_package_deal_does_not_charge_when_atomic_purchase_fails() {
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        10,
        Err(PackageDealFailure::Storage),
    );
    assert!(!outcome.charged_atomically);
    assert!(
        outcome
            .response
            .content
            .to_lowercase()
            .contains("not charged")
    );
}

#[test]
fn test_handle_package_deal_existing_active_deal_keeps_normal_price() {
    let normal_cost = 800;
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        10,
        Ok(package_success(normal_cost, 1, 10)),
    );
    assert_eq!(outcome.active_cost, normal_cost);
    assert!(
        outcome
            .response
            .content
            .contains(&format!("**Cost:** {normal_cost}"))
    );
}

#[test]
fn test_handle_package_deal_partial_top_up_reports_games_added() {
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        10,
        Ok(package_success(800, 1, 1)),
    );
    assert!(outcome.response.content.contains("**Games added:** 1"));
    assert!(outcome.response.content.contains("**Games remaining:** 10"));
    assert!(outcome.response.content.contains("**Cost:** 800"));
}

#[test]
fn test_handle_package_deal_clamps_purchase_duration_to_ten() {
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        25,
        Ok(package_success(1, 0, 10)),
    );
    assert_eq!(outcome.requested_games, 10);
}

#[test]
fn test_handle_package_deal_insufficient_balance_uses_atomic_quote() {
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        10,
        Err(PackageDealFailure::InsufficientBalance { balance: 200 }),
    );
    assert!(outcome.response.content.contains("need 800"));
    assert!(outcome.response.content.contains("only have 200"));
    assert!(!outcome.charged_atomically);
}

#[test]
fn test_handle_package_deal_already_full_does_not_charge() {
    let outcome = package_deal_command_outcome(
        "Target",
        Some(1500.0),
        Some(1500.0),
        10,
        Err(PackageDealFailure::AlreadyFull {
            games_remaining: 10,
        }),
    );
    assert!(!outcome.charged_atomically);
    assert!(
        outcome
            .response
            .content
            .contains("already has 10 games remaining")
    );
    assert!(
        outcome
            .response
            .content
            .to_lowercase()
            .contains("not charged")
    );
}

#[test]
fn test_handle_mystery_gift_requires_registration() {
    let outcome = simple_purchase(SimpleProduct::MysteryGift, 1001, false, 0, false, true);
    assert!(outcome.response.content.contains("`/player register`"));
    assert!(outcome.spend.is_none());
}

#[test]
fn test_handle_mystery_gift_insufficient_balance() {
    let outcome = simple_purchase(
        SimpleProduct::MysteryGift,
        1001,
        true,
        SHOP_NEW_MYSTERY_GIFT_COST - 1,
        false,
        true,
    );
    assert!(outcome.response.content.contains("You need"));
    assert!(outcome.response.content.contains("only have"));
}

#[test]
fn test_handle_mystery_gift_success_deducts_20k() {
    let outcome = simple_purchase(
        SimpleProduct::MysteryGift,
        1001,
        true,
        SHOP_NEW_MYSTERY_GIFT_COST + 100,
        true,
        true,
    );
    let spend = outcome.spend.unwrap();
    assert_eq!(spend.amount, 20_000);
    assert_eq!(spend.source, "shop_mystery_gift");
    assert!(
        outcome
            .response
            .embed_title
            .unwrap()
            .contains("Arcana/Immortal Gift")
    );
}

#[test]
fn test_handle_mystery_gift_aborts_when_balance_vanishes_before_the_debit() {
    let outcome = simple_purchase(
        SimpleProduct::MysteryGift,
        1001,
        true,
        SHOP_NEW_MYSTERY_GIFT_COST + 100,
        false,
        true,
    );
    assert!(!outcome.delivered);
    assert_eq!(outcome.response.phase, ResponsePhase::Initial);
    assert!(outcome.response.content.contains("no longer have"));
}

#[test]
fn test_handle_mystery_gift_shortfall_tolerates_lapsed_interaction() {
    let outcome = simple_purchase(
        SimpleProduct::MysteryGift,
        1001,
        true,
        SHOP_NEW_MYSTERY_GIFT_COST + 100,
        false,
        false,
    );
    assert!(outcome.notice_transport_lapsed);
    assert!(!outcome.delivered);
}

#[test]
fn test_handle_recalibrate_success() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::Allowed,
        2000,
        true,
        true,
        true,
    );
    assert!(outcome.recalibrated);
    assert_eq!(outcome.purchase.spend.unwrap().source, "shop_recalibrate");
    assert!(outcome.refund.is_none());
}

#[test]
fn test_handle_recalibrate_aborts_when_balance_vanishes_before_the_debit() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::Allowed,
        2000,
        false,
        true,
        true,
    );
    assert!(!outcome.recalibrated);
    assert!(!outcome.purchase.delivered);
    assert!(outcome.purchase.response.content.contains("no longer have"));
    assert!(outcome.refund.is_none());
}

#[test]
fn test_handle_recalibrate_shortfall_tolerates_lapsed_interaction() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::Allowed,
        2000,
        false,
        true,
        false,
    );
    assert!(outcome.purchase.notice_transport_lapsed);
    assert!(!outcome.recalibrated);
}

#[test]
fn test_handle_recalibrate_failure_refunds_with_ledger_context() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::Allowed,
        2000,
        true,
        false,
        true,
    );
    assert!(!outcome.recalibrated);
    assert_eq!(
        outcome.refund,
        Some(SpendIntent {
            amount: SHOP_RECALIBRATE_COST,
            source: "shop_recalibrate",
            actor_id: 1001,
        })
    );
}

#[test]
fn test_handle_recalibrate_on_cooldown() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::OnCooldown {
            ends_at: 9_999_999_999,
        },
        2000,
        false,
        false,
        true,
    );
    assert!(
        outcome
            .purchase
            .response
            .content
            .to_lowercase()
            .contains("cooldown")
    );
    assert!(outcome.purchase.spend.is_none());
}

#[test]
fn test_handle_recalibrate_insufficient_balance() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::Allowed,
        100,
        false,
        false,
        true,
    );
    assert!(outcome.purchase.response.content.contains("300"));
    assert!(outcome.purchase.response.content.contains("100"));
    assert!(outcome.purchase.spend.is_none());
}

#[test]
fn test_handle_recalibrate_not_registered() {
    let outcome = recalibration_purchase(
        1001,
        RecalibrationEligibility::NotRegistered,
        0,
        false,
        false,
        true,
    );
    assert!(
        outcome
            .purchase
            .response
            .content
            .to_lowercase()
            .contains("register")
    );
    assert!(outcome.purchase.spend.is_none());
}

#[test]
fn test_item_autocomplete_shows_cooldown() {
    let choices = item_autocomplete("", 10, true);
    let recalibrations: Vec<_> = choices
        .iter()
        .filter(|choice| choice.value.contains("recalibrate"))
        .collect();
    assert_eq!(recalibrations.len(), 1);
    assert_eq!(recalibrations[0].value, "recalibrate_cooldown");
    assert!(recalibrations[0].name.contains("ON COOLDOWN"));
}

#[test]
fn test_item_autocomplete_shows_price_when_available() {
    let choice = item_autocomplete("", 10, false)
        .into_iter()
        .find(|choice| choice.value == "recalibrate")
        .unwrap();
    assert!(choice.name.contains("300"));
}

#[test]
fn test_item_autocomplete_excludes_curse_and_includes_jopa_coin() {
    let values: Vec<_> = item_autocomplete("", 10, false)
        .into_iter()
        .map(|choice| choice.value)
        .collect();
    assert!(values.contains(&"jopa_coin"));
    assert!(values.contains(&"mystery_gift"));
    assert!(!values.contains(&"witchs_curse"));
}

#[test]
fn test_double_or_nothing_rejects_balance_equal_to_cost() {
    let store = InMemoryShopSettlement::new();
    store.register(1001, 9001, SHOP_DOUBLE_OR_NOTHING_COST);
    let result = store
        .settle_double_or_nothing(
            DoubleOrNothingRequest {
                account_id: 1001,
                guild_id: 9001,
                cost: SHOP_DOUBLE_OR_NOTHING_COST,
                won: true,
                now: 1_700_000_000,
                cooldown_seconds: DOUBLE_OR_NOTHING_COOLDOWN_SECONDS,
                bypass_cooldown: false,
            },
            true,
        )
        .unwrap();
    assert_eq!(result.reason, Some(PurchaseFailureReason::NothingToDouble));
    assert_eq!(store.balance(1001, 9001), Some(SHOP_DOUBLE_OR_NOTHING_COST));
    assert!(store.spins().is_empty());
}

#[test]
fn test_item_autocomplete_excludes_dig_items() {
    assert!(
        item_autocomplete("", 10, false)
            .iter()
            .all(|choice| !choice.value.starts_with("dig_"))
    );
}

#[test]
fn test_regrowth_recovers_losses_within_24h_even_before_4am_reset() {
    let now = 1_779_293_400;
    let records = [LossRecord {
        account_id: 4242,
        guild_id: 9000,
        occurred_at: now - 12 * 3_600,
        kind: LossKind::Bet {
            won: false,
            amount: 200,
            leverage: 5,
        },
    }];
    let losses = recent_loss_aggregates(&records, 4242, 9000, now - 24 * 3_600);
    let recovery = generated_recovery(losses.total, 0.35, 120, Some(9000), None);
    assert_eq!(losses.total, 1_000);
    assert_eq!(recovery.base_reward, 120);
    assert_eq!(recovery.adjusted_reward, 120);
}

#[test]
fn test_recent_loss_aggregate_combines_only_recent_losing_bets_and_wheel() {
    let account_id = 4242;
    let guild_id = 9000;
    let cutoff = 1_000_000;
    let records = [
        LossRecord {
            account_id,
            guild_id,
            occurred_at: cutoff + 1,
            kind: LossKind::Bet {
                won: false,
                amount: 10,
                leverage: 5,
            },
        },
        LossRecord {
            account_id,
            guild_id,
            occurred_at: cutoff + 2,
            kind: LossKind::Bet {
                won: true,
                amount: 100,
                leverage: 5,
            },
        },
        LossRecord {
            account_id,
            guild_id,
            occurred_at: cutoff + 3,
            kind: LossKind::Bet {
                won: false,
                amount: 30,
                leverage: 1,
            },
        },
        LossRecord {
            account_id,
            guild_id,
            occurred_at: cutoff - 1,
            kind: LossKind::Bet {
                won: false,
                amount: 999,
                leverage: 1,
            },
        },
        LossRecord {
            account_id,
            guild_id: guild_id + 1,
            occurred_at: cutoff + 4,
            kind: LossKind::Bet {
                won: false,
                amount: 999,
                leverage: 1,
            },
        },
        LossRecord {
            account_id,
            guild_id,
            occurred_at: cutoff + 5,
            kind: LossKind::Wheel { result: -70 },
        },
        LossRecord {
            account_id,
            guild_id,
            occurred_at: cutoff + 6,
            kind: LossKind::Wheel { result: 500 },
        },
    ];
    assert_eq!(
        recent_loss_aggregates(&records, account_id, guild_id, cutoff),
        LossAggregates {
            largest: 70,
            total: 150,
        }
    );
}

struct RewardEvent {
    received: Option<(Option<i64>, i64)>,
    result: i64,
}

impl DailyRewardPolicy for RewardEvent {
    fn adjust_reward(&mut self, guild_id: Option<i64>, amount: i64) -> i64 {
        self.received = Some((guild_id, amount));
        self.result
    }
}

#[test]
fn test_manashop_generated_recovery_scales_before_daily_event_mana_shield() {
    let mut event = RewardEvent {
        received: None,
        result: 41,
    };
    let reward = generated_recovery(1_000, 0.60, 120, Some(9000), Some(&mut event));
    assert_eq!(reward.base_reward, 120);
    assert_eq!(event.received, Some((Some(9000), 120)));
    assert_eq!(reward.adjusted_reward, 41);
}

#[test]
fn test_manashop_generated_recovery_scales_before_daily_event_regrowth() {
    let mut event = RewardEvent {
        received: None,
        result: 41,
    };
    let reward = generated_recovery(1_000, 0.35, 120, Some(9000), Some(&mut event));
    assert_eq!(reward.base_reward, 120);
    assert_eq!(event.received, Some((Some(9000), 120)));
    assert_eq!(reward.adjusted_reward, 41);
}

#[test]
fn test_double_or_nothing_atomic_settlement_blocks_concurrent_second_spin() {
    let store = InMemoryShopSettlement::new();
    store.register(1001, 9001, 200);
    let request = DoubleOrNothingRequest {
        account_id: 1001,
        guild_id: 9001,
        cost: SHOP_DOUBLE_OR_NOTHING_COST,
        won: true,
        now: 1_700_000_000,
        cooldown_seconds: DOUBLE_OR_NOTHING_COOLDOWN_SECONDS,
        bypass_cooldown: false,
    };
    let first = store.settle_double_or_nothing(request, true).unwrap();
    let second = store.settle_double_or_nothing(request, true).unwrap();
    assert!(first.success);
    assert_eq!(first.balance_at_risk, 150);
    assert_eq!(first.balance_after, 300);
    assert_eq!(second.reason, Some(PurchaseFailureReason::OnCooldown));
    assert_eq!(store.spins().len(), 1);
    assert_eq!(store.balance(1001, 9001), Some(300));
}

#[test]
fn test_handle_announce_aborts_when_balance_vanishes_before_the_debit() {
    let outcome = simple_purchase(
        SimpleProduct::AnnounceTarget,
        1001,
        true,
        SHOP_ANNOUNCE_TARGET_COST + 10,
        false,
        true,
    );
    assert!(outcome.spend.is_some());
    assert!(!outcome.delivered);
    assert_eq!(outcome.response.phase, ResponsePhase::Initial);
    assert_eq!(outcome.response.visibility, Visibility::Ephemeral);
    assert!(outcome.response.content.contains("no longer have"));
}

#[test]
fn test_handle_announce_shortfall_tolerates_lapsed_interaction() {
    let outcome = simple_purchase(
        SimpleProduct::Announce,
        1001,
        true,
        SHOP_ANNOUNCE_COST + 10,
        false,
        false,
    );
    assert!(outcome.notice_transport_lapsed);
    assert!(!outcome.delivered);
    assert_eq!(outcome.response.phase, ResponsePhase::Initial);
}

fn shop_player(discord_id: i64, balance: i64) -> ShopPlayer {
    ShopPlayer {
        discord_id,
        name: format!("player {discord_id}"),
        wins: 0,
        losses: 0,
        glicko_rating: None,
        glicko_rd: None,
        glicko_volatility: None,
        os_mu: None,
        os_sigma: None,
        balance,
        lowest_balance: None,
        last_double_or_nothing: None,
    }
}

fn hostile_context(minigame_scale: f64) -> HostileEffectContext<'static> {
    HostileEffectContext {
        buyer_id: 4242,
        guild_id: 9001,
        purchase_id: "purchase-1",
        occurred_at: 1_700_000_000,
        mana_date: "2023-11-14",
        config: ManaShopEffectConfig {
            minigame_scale,
            hostile_min_balance: 60,
        },
    }
}

#[test]
fn pyroclasm_burns_deflationary_scaled_rolls_clamped_to_each_balance() {
    let context = hostile_context(1.0);
    let victims = [
        shop_player(5000, 100),
        shop_player(5001, 3),
        shop_player(5002, 0),
    ];
    let requests = pyroclasm_requests(&context, &victims, &[20, 20, 20]);
    let expected = scale_deflationary_minigame_jc_delta(20.0, 1.0);
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.victim_id, request.requested))
            .collect::<Vec<_>>(),
        // The broke victim scales to nothing and is skipped.
        vec![(5000, expected.min(100)), (5001, 3)]
    );
    let first = &requests[0];
    assert_eq!(first.kind, "pyroclasm");
    assert_eq!(first.event_key, "pyroclasm:purchase-1:5000");
    assert_eq!(first.destination, HostileDestination::Burn);
    assert_eq!(first.recipient_id, None);
    assert_eq!(first.actor_id, Some(4242));
    assert_eq!(first.min_balance, Some(60));
    assert!(first.clamp_to_balance);
    assert_eq!(first.mana_date, "2023-11-14");
    assert_eq!(first.metadata["destination"], "burn");
    // The deployment's minigame scale changes every burn.
    let doubled = pyroclasm_requests(&hostile_context(2.0), &victims[..1], &[20]);
    assert_eq!(
        doubled[0].requested,
        scale_deflationary_minigame_jc_delta(20.0, 2.0)
    );
}

#[test]
fn pyroclasm_bounty_is_half_the_burn_capped_and_wildfire_keeps_45_percent() {
    assert_eq!(pyroclasm_bounty(0), 0);
    assert_eq!(pyroclasm_bounty(41), 20);
    assert_eq!(pyroclasm_bounty(200), PYROCLASM_MAX_BOUNTY);
    assert_eq!(wildfire_harvest(100), 45);
    assert_eq!(wildfire_harvest(3), 1);
}

#[test]
fn soul_harvest_drains_the_scaled_base_and_bonus_into_the_buyer() {
    let context = hostile_context(1.0);
    let victims = [
        shop_player(5001, 100),
        shop_player(5002, 100),
        shop_player(5003, 2),
    ];
    let requests = soul_harvest_requests(&context, &victims, &[false, true, true]);
    let base = scale_minigame_jc_delta(SOUL_HARVEST_DRAIN_PER_TARGET as f64, 1.0);
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.victim_id, request.requested))
            .collect::<Vec<_>>(),
        vec![(5001, base), (5002, base + 1), (5003, (base + 1).min(2))]
    );
    assert!(requests.iter().all(|request| {
        request.destination == HostileDestination::Player
            && request.recipient_id == Some(4242)
            && request.event_key.starts_with("soul_harvest:purchase-1:")
    }));
}

#[test]
fn wildfire_burns_every_eligible_player_once() {
    let context = hostile_context(1.0);
    let victims = [shop_player(5001, 100), shop_player(5002, 100)];
    let requests = wildfire_requests(&context, &victims, &[4, 14]);
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.event_key.as_str(), request.requested))
            .collect::<Vec<_>>(),
        vec![
            (
                "wildfire:purchase-1:5001",
                scale_deflationary_minigame_jc_delta(4.0, 1.0)
            ),
            (
                "wildfire:purchase-1:5002",
                scale_deflationary_minigame_jc_delta(14.0, 1.0)
            ),
        ]
    );
}

#[test]
fn hostile_totals_sum_only_settled_losses() {
    let settled = |applied, absorbed| HostileLossSettlement {
        event_id: 1,
        event_key: "key".to_owned(),
        requested: applied + absorbed,
        attempted: applied + absorbed,
        absorbed,
        applied,
        victim_balance_before: 100,
        victim_balance_after: 100 - applied,
        destination_balance_before: None,
        destination_balance_after: None,
        duplicate: false,
        details: Vec::new(),
    };
    assert_eq!(
        hostile_totals(&[
            Ok(settled(10, 2)),
            Err("rejected".to_owned()),
            Ok(settled(5, 0)),
        ]),
        (15, 2)
    );
}

#[test]
fn mana_recovery_reads_the_right_loss_and_caps_at_120() {
    assert_eq!(mana_recovery_base("mana_shield", 100, 1_000), Some(60));
    assert_eq!(mana_recovery_base("mana_shield", 1_000, 1_000), Some(120));
    assert_eq!(mana_recovery_base("regrowth", 1_000, 100), Some(35));
    assert_eq!(mana_recovery_base("regrowth", 0, 10_000), Some(120));
    assert_eq!(mana_recovery_base("aegis", 100, 100), None);
}
