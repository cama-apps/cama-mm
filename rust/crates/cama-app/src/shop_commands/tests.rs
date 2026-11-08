use std::sync::{Arc, Barrier};
use std::thread;

use super::*;

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
    assert_eq!(losses.total, 1_000);
    assert_eq!(
        mana_recovery_base("regrowth", losses.largest, losses.total),
        Some(120)
    );
}
