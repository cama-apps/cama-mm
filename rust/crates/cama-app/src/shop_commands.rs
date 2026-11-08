//! Discord-independent shop command policy and atomic settlement seams.
//!
//! This module keeps the command's validation order, visibility decisions,
//! pricing, and transaction boundaries independent from the Discord
//! transport.  Adapters implement the ports below without being able to
//! write the same purchase twice.

use std::collections::HashMap;
use std::sync::Mutex;

use cama_db::shop_runtime::{
    HostileDestination, HostileLossRequest, HostileLossSettlement, ShopPlayer,
};
use cama_domain::economy_scaling::{scale_deflationary_minigame_jc_delta, scale_minigame_jc_delta};
use serde_json::json;

pub const SHOP_ANNOUNCE_COST: i64 = 10;
pub const SHOP_ANNOUNCE_TARGET_COST: i64 = 100;
pub const SHOP_JOPA_COIN_COST: i64 = 10_000;
pub const SHOP_NEW_MYSTERY_GIFT_COST: i64 = 20_000;
pub const SHOP_DOUBLE_OR_NOTHING_COST: i64 = 50;
pub const SHOP_RECALIBRATE_COST: i64 = 300;
pub const SHOP_SOFT_AVOID_COST: i64 = 500;
pub const SOFT_AVOID_MIN_COST: i64 = 250;
pub const SOFT_AVOID_WINRATE_COST_SCALE: i64 = 1_000;
pub const SOFT_AVOID_MIN_TEAMMATE_GAMES: i64 = 3;
pub const SOFT_AVOID_GAMES: i64 = 10;
pub const PACKAGE_DEAL_MAX_GAMES: i64 = 10;
pub const PACKAGE_DEAL_NO_ACTIVE_COST: i64 = 1;
pub const PINGEDASH_COST: i64 = 10;
pub const PINGEDKEVIN_COST: i64 = PINGEDASH_COST;
pub const PINGEDASH_COOLDOWN_SECONDS: i64 = 86_400;
pub const DOUBLE_OR_NOTHING_COOLDOWN_SECONDS: i64 = 2_592_000;
pub const PINGEDASH_TENOR_URL: &str = "https://tenor.com/view/hiash-gif-25282310";
pub const PINGEDKEVIN_TENOR_URL: &str =
    "https://tenor.com/view/in-trouble-mad-face-gif-10963449859613356314";
pub const HOSTILE_LOSS_MIN_BALANCE: i64 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Ephemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponsePhase {
    Initial,
    FollowupAfterPublicDefer,
    FollowupAfterEphemeralDefer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResponse {
    pub content: String,
    pub visibility: Visibility,
    pub phase: ResponsePhase,
    pub embed_title: Option<String>,
    pub allowed_users: Vec<i64>,
}

impl CommandResponse {
    fn initial_ephemeral(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            visibility: Visibility::Ephemeral,
            phase: ResponsePhase::Initial,
            embed_title: None,
            allowed_users: Vec::new(),
        }
    }

    fn public_followup(content: impl Into<String>, embed_title: Option<&str>) -> Self {
        Self {
            content: content.into(),
            visibility: Visibility::Public,
            phase: ResponsePhase::FollowupAfterPublicDefer,
            embed_title: embed_title.map(str::to_owned),
            allowed_users: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaidPingKind {
    Dash,
    Kevin,
}

impl PaidPingKind {
    #[must_use]
    pub const fn command_name(self) -> &'static str {
        match self {
            Self::Dash => "pingedash",
            Self::Kevin => "pingedkevin",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Dash => "Pingedash",
            Self::Kevin => "PingedKevin",
        }
    }

    #[must_use]
    pub const fn tenor_url(self) -> &'static str {
        match self {
            Self::Dash => PINGEDASH_TENOR_URL,
            Self::Kevin => PINGEDKEVIN_TENOR_URL,
        }
    }

    #[must_use]
    pub fn description(self) -> String {
        let cost = match self {
            Self::Dash => PINGEDASH_COST,
            Self::Kevin => PINGEDKEVIN_COST,
        };
        format!("Spend {cost} jopacoin to send the {}", self.display_name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurchaseFailureReason {
    NotRegistered,
    OnCooldown,
    InsufficientBalance,
    NothingToDouble,
    InDebt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaidPingPurchase {
    pub success: bool,
    pub reason: Option<PurchaseFailureReason>,
    pub balance: i64,
    pub cooldown_ends_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    pub account_id: i64,
    pub guild_id: i64,
    pub delta: i64,
    pub source: String,
    pub related_type: String,
    pub related_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoubleOrNothingSpin {
    pub account_id: i64,
    pub guild_id: i64,
    pub cost: i64,
    pub balance_at_risk: i64,
    pub balance_after: i64,
    pub won: bool,
    pub spin_time: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoubleOrNothingRequest {
    pub account_id: i64,
    pub guild_id: i64,
    pub cost: i64,
    pub won: bool,
    pub now: i64,
    pub cooldown_seconds: i64,
    pub bypass_cooldown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DoubleOrNothingSettlement {
    pub success: bool,
    pub reason: Option<PurchaseFailureReason>,
    pub balance_before: i64,
    pub balance_at_risk: i64,
    pub balance_after: i64,
    pub won: bool,
    pub cooldown_ends_at: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct AccountState {
    balance: i64,
    last_double_or_nothing: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct SettlementState {
    accounts: HashMap<(i64, i64), AccountState>,
    ledger: Vec<LedgerEntry>,
    spins: Vec<DoubleOrNothingSpin>,
}

/// A deterministic, mutex-serialized implementation of the command's atomic
/// semantics.  It is suitable for parity tests; production uses the same API
/// through a SQLite adapter against Python's existing tables.
#[derive(Debug, Default)]
pub struct InMemoryShopSettlement {
    state: Mutex<SettlementState>,
}

impl InMemoryShopSettlement {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, account_id: i64, guild_id: i64, balance: i64) {
        self.state
            .lock()
            .expect("shop settlement mutex poisoned")
            .accounts
            .insert(
                (account_id, guild_id),
                AccountState {
                    balance,
                    ..AccountState::default()
                },
            );
    }

    pub fn set_balance(&self, account_id: i64, guild_id: i64, balance: i64) {
        if let Some(account) = self
            .state
            .lock()
            .expect("shop settlement mutex poisoned")
            .accounts
            .get_mut(&(account_id, guild_id))
        {
            account.balance = balance;
        }
    }

    #[must_use]
    pub fn balance(&self, account_id: i64, guild_id: i64) -> Option<i64> {
        self.state
            .lock()
            .expect("shop settlement mutex poisoned")
            .accounts
            .get(&(account_id, guild_id))
            .map(|account| account.balance)
    }

    #[must_use]
    pub fn ledger(&self) -> Vec<LedgerEntry> {
        self.state
            .lock()
            .expect("shop settlement mutex poisoned")
            .ledger
            .clone()
    }

    #[must_use]
    pub fn spins(&self) -> Vec<DoubleOrNothingSpin> {
        self.state
            .lock()
            .expect("shop settlement mutex poisoned")
            .spins
            .clone()
    }

    /// Settle the entire gamble under one lock.  `persist_spin=false` models a
    /// failed spin-row insert and proves that no balance/cooldown mutation leaks.
    pub fn settle_double_or_nothing(
        &self,
        request: DoubleOrNothingRequest,
        persist_spin: bool,
    ) -> Result<DoubleOrNothingSettlement, &'static str> {
        let mut state = self.state.lock().expect("shop settlement mutex poisoned");
        let Some(account_snapshot) = state
            .accounts
            .get(&(request.account_id, request.guild_id))
            .cloned()
        else {
            return Ok(rejected_double(
                PurchaseFailureReason::NotRegistered,
                0,
                request.won,
                None,
            ));
        };

        if let Some(last_used) = account_snapshot.last_double_or_nothing {
            let cooldown_ends_at = last_used.saturating_add(request.cooldown_seconds);
            if !request.bypass_cooldown && request.now < cooldown_ends_at {
                return Ok(rejected_double(
                    PurchaseFailureReason::OnCooldown,
                    account_snapshot.balance,
                    request.won,
                    Some(cooldown_ends_at),
                ));
            }
        }
        let reason = if account_snapshot.balance < 0 {
            Some(PurchaseFailureReason::InDebt)
        } else if account_snapshot.balance < request.cost {
            Some(PurchaseFailureReason::InsufficientBalance)
        } else if account_snapshot.balance == request.cost {
            Some(PurchaseFailureReason::NothingToDouble)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Ok(rejected_double(
                reason,
                account_snapshot.balance,
                request.won,
                None,
            ));
        }
        if !persist_spin {
            return Err("forced log failure");
        }

        let balance_before = account_snapshot.balance;
        let balance_at_risk = balance_before - request.cost;
        let balance_after = if request.won {
            balance_at_risk.saturating_mul(2)
        } else {
            0
        };
        let account = state
            .accounts
            .get_mut(&(request.account_id, request.guild_id))
            .expect("account existed in snapshot");
        account.balance = balance_after;
        account.last_double_or_nothing = Some(request.now);
        state.spins.push(DoubleOrNothingSpin {
            account_id: request.account_id,
            guild_id: request.guild_id,
            cost: request.cost,
            balance_at_risk,
            balance_after,
            won: request.won,
            spin_time: request.now,
        });
        state.ledger.push(LedgerEntry {
            account_id: request.account_id,
            guild_id: request.guild_id,
            delta: balance_after - balance_before,
            source: "double_or_nothing".to_owned(),
            related_type: "command".to_owned(),
            related_id: "double_or_nothing".to_owned(),
        });
        Ok(DoubleOrNothingSettlement {
            success: true,
            reason: None,
            balance_before,
            balance_at_risk,
            balance_after,
            won: request.won,
            cooldown_ends_at: Some(request.now.saturating_add(request.cooldown_seconds)),
        })
    }
}

fn rejected_double(
    reason: PurchaseFailureReason,
    balance: i64,
    won: bool,
    cooldown_ends_at: Option<i64>,
) -> DoubleOrNothingSettlement {
    DoubleOrNothingSettlement {
        success: false,
        reason: Some(reason),
        balance_before: balance,
        balance_at_risk: balance,
        balance_after: balance,
        won,
        cooldown_ends_at,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub name: &'static str,
    pub value: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimpleProduct {
    Announce,
    AnnounceTarget,
    JopaCoin,
    MysteryGift,
    Recalibrate,
}

impl SimpleProduct {
    #[must_use]
    pub const fn cost(self) -> i64 {
        match self {
            Self::Announce => SHOP_ANNOUNCE_COST,
            Self::AnnounceTarget => SHOP_ANNOUNCE_TARGET_COST,
            Self::JopaCoin => SHOP_JOPA_COIN_COST,
            Self::MysteryGift => SHOP_NEW_MYSTERY_GIFT_COST,
            Self::Recalibrate => SHOP_RECALIBRATE_COST,
        }
    }

    #[must_use]
    pub const fn source(self) -> &'static str {
        match self {
            Self::Announce | Self::AnnounceTarget => "shop_announce",
            Self::JopaCoin => "shop_jopa_coin",
            Self::MysteryGift => "shop_mystery_gift",
            Self::Recalibrate => "shop_recalibrate",
        }
    }

    const fn embed_title(self) -> &'static str {
        match self {
            Self::Announce | Self::AnnounceTarget => "WEALTH ANNOUNCEMENT",
            Self::JopaCoin => "Jopa Coin(TM) Minted!",
            Self::MysteryGift => "Arcana/Immortal Gift Redeemed!",
            Self::Recalibrate => "Rating Recalibration",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendIntent {
    pub amount: i64,
    pub source: &'static str,
    pub actor_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimplePurchaseOutcome {
    pub response: CommandResponse,
    pub spend: Option<SpendIntent>,
    pub delivered: bool,
    pub notice_transport_lapsed: bool,
}

#[must_use]
pub fn simple_purchase(
    product: SimpleProduct,
    user_id: i64,
    registered: bool,
    observed_balance: i64,
    debit_committed: bool,
    notice_transport_alive: bool,
) -> SimplePurchaseOutcome {
    let cost = product.cost();
    if !registered {
        return SimplePurchaseOutcome {
            response: CommandResponse::initial_ephemeral(
                "You need to `/player register` before you can shop.",
            ),
            spend: None,
            delivered: false,
            notice_transport_lapsed: !notice_transport_alive,
        };
    }
    if observed_balance < cost {
        return SimplePurchaseOutcome {
            response: CommandResponse::initial_ephemeral(format!(
                "You need {cost} jopacoin for this, but you only have {observed_balance}."
            )),
            spend: None,
            delivered: false,
            notice_transport_lapsed: !notice_transport_alive,
        };
    }
    let spend = Some(SpendIntent {
        amount: cost,
        source: product.source(),
        actor_id: user_id,
    });
    if !debit_committed {
        return SimplePurchaseOutcome {
            response: CommandResponse::initial_ephemeral(format!(
                "You no longer have {cost} jopacoin for this."
            )),
            spend,
            delivered: false,
            notice_transport_lapsed: !notice_transport_alive,
        };
    }
    SimplePurchaseOutcome {
        response: CommandResponse::public_followup("", Some(product.embed_title())),
        spend,
        delivered: true,
        notice_transport_lapsed: false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecalibrationEligibility {
    Allowed,
    NotRegistered,
    NoRating,
    InsufficientGames { games_played: i64, min_games: i64 },
    OnCooldown { ends_at: i64 },
}

#[must_use]
pub fn soft_avoid_choice_name(configured_default: i64) -> String {
    let default_cost = configured_default.max(SOFT_AVOID_MIN_COST);
    format!(
        "Soft Avoid ({default_cost} before {SOFT_AVOID_MIN_TEAMMATE_GAMES} shared games; {SOFT_AVOID_MIN_COST}-{SOFT_AVOID_WINRATE_COST_SCALE} by win rate; lasts {SOFT_AVOID_GAMES} games)"
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PairingRecord {
    pub games_together: i64,
    pub wins_together: i64,
}

#[must_use]
pub fn calculate_soft_avoid_cost(pairing: Option<PairingRecord>, configured_default: i64) -> i64 {
    let default_cost = configured_default.max(SOFT_AVOID_MIN_COST);
    let Some(pairing) = pairing else {
        return default_cost;
    };
    if pairing.games_together < SOFT_AVOID_MIN_TEAMMATE_GAMES {
        return default_cost;
    }
    let numerator = SOFT_AVOID_WINRATE_COST_SCALE
        .saturating_mul(pairing.wins_together)
        .saturating_add(pairing.games_together - 1);
    (numerator / pairing.games_together).max(SOFT_AVOID_MIN_COST)
}

#[must_use]
pub fn package_deal_games(configured_duration: i64) -> i64 {
    configured_duration.min(PACKAGE_DEAL_MAX_GAMES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManaTier {
    Cheap,
    Mid,
    Ultimate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManaItemSpec {
    pub key: &'static str,
    pub name: &'static str,
    pub color: &'static str,
    pub cost: i64,
    pub tier: ManaTier,
    pub target_required: bool,
    pub self_target_allowed: bool,
}

pub const MANA_ITEMS: &[ManaItemSpec] = &[
    ManaItemSpec {
        key: "pyroclasm",
        name: "Pyroclasm",
        color: "Red",
        cost: 25,
        tier: ManaTier::Cheap,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "insight",
        name: "Insight",
        color: "Blue",
        cost: 10,
        tier: ManaTier::Cheap,
        target_required: true,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "sapling",
        name: "Sapling",
        color: "Green",
        cost: 10,
        tier: ManaTier::Cheap,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "reprieve",
        name: "Reprieve",
        color: "White",
        cost: 15,
        tier: ManaTier::Cheap,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "soul_harvest",
        name: "Soul Harvest",
        color: "Black",
        cost: 25,
        tier: ManaTier::Cheap,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "dynamite_cache",
        name: "Dynamite Cache",
        color: "Red",
        cost: 35,
        tier: ManaTier::Mid,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "mana_shield",
        name: "Mana Shield",
        color: "Blue",
        cost: 35,
        tier: ManaTier::Mid,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "regrowth",
        name: "Regrowth",
        color: "Green",
        cost: 25,
        tier: ManaTier::Mid,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "aegis",
        name: "Aegis",
        color: "White",
        cost: 35,
        tier: ManaTier::Mid,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "blood_pact",
        name: "Blood Pact",
        color: "Black",
        cost: 75,
        tier: ManaTier::Mid,
        target_required: true,
        self_target_allowed: false,
    },
    ManaItemSpec {
        key: "wildfire",
        name: "Wildfire",
        color: "Red",
        cost: 150,
        tier: ManaTier::Ultimate,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "counterspell",
        name: "Counterspell",
        color: "Blue",
        cost: 75,
        tier: ManaTier::Ultimate,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "overgrowth",
        name: "Overgrowth",
        color: "Green",
        cost: 90,
        tier: ManaTier::Ultimate,
        target_required: false,
        self_target_allowed: true,
    },
    ManaItemSpec {
        key: "sanctuary",
        name: "Sanctuary",
        color: "White",
        cost: 90,
        tier: ManaTier::Ultimate,
        target_required: true,
        self_target_allowed: false,
    },
    ManaItemSpec {
        key: "dark_bargain",
        name: "Dark Bargain",
        color: "Black",
        cost: 150,
        tier: ManaTier::Ultimate,
        target_required: false,
        self_target_allowed: true,
    },
];

#[must_use]
pub fn mana_item(key: &str) -> Option<ManaItemSpec> {
    MANA_ITEMS.iter().find(|item| item.key == key).copied()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManaPurchaseFailure {
    AlreadyUsed,
    PurchaseInProgress,
    ManaTapped,
    NotRegistered,
    InsufficientBalance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossKind {
    Bet {
        won: bool,
        amount: i64,
        leverage: i64,
    },
    Wheel {
        result: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LossRecord {
    pub account_id: i64,
    pub guild_id: i64,
    pub occurred_at: i64,
    pub kind: LossKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LossAggregates {
    pub largest: i64,
    pub total: i64,
}

#[must_use]
pub fn recent_loss_aggregates(
    records: &[LossRecord],
    account_id: i64,
    guild_id: i64,
    cutoff: i64,
) -> LossAggregates {
    records
        .iter()
        .filter(|record| {
            record.account_id == account_id
                && record.guild_id == guild_id
                && record.occurred_at >= cutoff
        })
        .filter_map(|record| match record.kind {
            LossKind::Bet {
                won: false,
                amount,
                leverage,
            } => Some(amount.saturating_mul(leverage)),
            LossKind::Wheel { result } if result < 0 => Some(result.saturating_abs()),
            _ => None,
        })
        .fold(LossAggregates::default(), |mut aggregate, loss| {
            aggregate.largest = aggregate.largest.max(loss);
            aggregate.total = aggregate.total.saturating_add(loss);
            aggregate
        })
}

/// Settings the hostile mana-shop effects read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManaShopEffectConfig {
    pub minigame_scale: f64,
    /// Victims below this balance are never charged.
    pub hostile_min_balance: i64,
}

/// The purchase a hostile mana-shop effect settles for.
#[derive(Clone, Copy, Debug)]
pub struct HostileEffectContext<'a> {
    pub buyer_id: i64,
    pub guild_id: i64,
    pub purchase_id: &'a str,
    pub occurred_at: i64,
    /// The Pacific mana day the protection pools are read for.
    pub mana_date: &'a str,
    pub config: ManaShopEffectConfig,
}

/// Pyroclasm scorches up to this many randomly sampled players.
pub const PYROCLASM_TARGETS: usize = 3;
/// Inclusive range of each Pyroclasm burn roll, before deflationary scaling.
pub const PYROCLASM_ROLL_RANGE: (i64, i64) = (12, 28);
/// The buyer keeps half the burned total, up to this many JopaCoin.
pub const PYROCLASM_MAX_BOUNTY: i64 = 35;
/// Inclusive range of each Wildfire burn roll, before deflationary scaling.
pub const WILDFIRE_ROLL_RANGE: (i64, i64) = (4, 14);
/// Soul Harvest drains this much from every eligible player, before scaling.
pub const SOUL_HARVEST_DRAIN_PER_TARGET: i64 = 2;
/// Chance that Soul Harvest drains one extra JopaCoin from a player.
pub const SOUL_HARVEST_BONUS_DRAIN_CHANCE: f64 = 0.20;
/// Reprieve reconciles hostile losses from this far back.
pub const REPRIEVE_LOOKBACK_SECONDS: i64 = 24 * 3_600;
/// Mana Shield and Regrowth recover at most this much.
pub const MANA_RECOVERY_CAP: i64 = 120;
/// Dark Bargain pays this principal now...
pub const DARK_BARGAIN_PRINCIPAL: i64 = 800;
/// ...and owes this much in seven days.
pub const DARK_BARGAIN_AMOUNT_DUE: i64 = 700;

/// One hostile mana-shop loss against `victim_id`, keyed to the purchase so a
/// retried settlement reuses it.
#[must_use]
pub fn hostile_effect_request(
    context: &HostileEffectContext<'_>,
    victim_id: i64,
    requested: i64,
    kind: &str,
    destination: HostileDestination,
    recipient_id: Option<i64>,
) -> HostileLossRequest {
    HostileLossRequest {
        victim_id,
        guild_id: context.guild_id,
        requested,
        kind: kind.to_owned(),
        actor_id: Some(context.buyer_id),
        event_key: format!("{kind}:{}:{victim_id}", context.purchase_id),
        destination,
        recipient_id,
        clamp_to_balance: true,
        min_balance: Some(context.config.hostile_min_balance),
        protect_self: false,
        metadata: json!({
            "kind": kind,
            "attempted_loss": requested,
            "destination": match destination {
                HostileDestination::Burn => "burn",
                HostileDestination::Player => "player",
                HostileDestination::Reserve => "reserve",
            },
            "recipient_id": recipient_id,
        }),
        occurred_at: context.occurred_at,
        mana_date: context.mana_date.to_owned(),
    }
}

/// Burn requests for one roll per sampled victim. A roll that scales to
/// nothing, or a victim with nothing left, is skipped.
fn burn_requests(
    context: &HostileEffectContext<'_>,
    victims: &[ShopPlayer],
    rolls: &[i64],
    kind: &str,
) -> Vec<HostileLossRequest> {
    victims
        .iter()
        .zip(rolls)
        .filter_map(|(victim, roll)| {
            let amount =
                scale_deflationary_minigame_jc_delta(*roll as f64, context.config.minigame_scale)
                    .min(victim.balance)
                    .max(0);
            (amount > 0).then(|| {
                hostile_effect_request(
                    context,
                    victim.discord_id,
                    amount,
                    kind,
                    HostileDestination::Burn,
                    None,
                )
            })
        })
        .collect()
}

/// Pyroclasm: burn one [`PYROCLASM_ROLL_RANGE`] roll from each of the sampled
/// victims.
#[must_use]
pub fn pyroclasm_requests(
    context: &HostileEffectContext<'_>,
    victims: &[ShopPlayer],
    rolls: &[i64],
) -> Vec<HostileLossRequest> {
    burn_requests(context, victims, rolls, "pyroclasm")
}

/// The buyer's share of what Pyroclasm actually burned.
#[must_use]
pub const fn pyroclasm_bounty(burned: i64) -> i64 {
    let half = burned / 2;
    if half < PYROCLASM_MAX_BOUNTY {
        half
    } else {
        PYROCLASM_MAX_BOUNTY
    }
}

/// Wildfire: burn one [`WILDFIRE_ROLL_RANGE`] roll from every eligible player.
#[must_use]
pub fn wildfire_requests(
    context: &HostileEffectContext<'_>,
    victims: &[ShopPlayer],
    rolls: &[i64],
) -> Vec<HostileLossRequest> {
    burn_requests(context, victims, rolls, "wildfire")
}

/// The buyer's 45% share of what Wildfire actually burned.
#[must_use]
pub fn wildfire_harvest(burned: i64) -> i64 {
    (burned as f64 * 0.45) as i64
}

/// Soul Harvest: drain the scaled base, plus one when that player's bonus
/// draw hit, from every eligible player into the buyer.
#[must_use]
pub fn soul_harvest_requests(
    context: &HostileEffectContext<'_>,
    victims: &[ShopPlayer],
    bonus_drains: &[bool],
) -> Vec<HostileLossRequest> {
    let base = scale_minigame_jc_delta(
        SOUL_HARVEST_DRAIN_PER_TARGET as f64,
        context.config.minigame_scale,
    );
    victims
        .iter()
        .zip(bonus_drains)
        .map(|(victim, bonus)| {
            hostile_effect_request(
                context,
                victim.discord_id,
                (base + i64::from(*bonus)).min(victim.balance),
                "soul_harvest",
                HostileDestination::Player,
                Some(context.buyer_id),
            )
        })
        .collect()
}

/// Applied and shield-absorbed totals across one batch of settlements.
#[must_use]
pub fn hostile_totals(outcomes: &[Result<HostileLossSettlement, String>]) -> (i64, i64) {
    outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok())
        .fold((0, 0), |(applied, absorbed), outcome| {
            (
                applied.saturating_add(outcome.applied),
                absorbed.saturating_add(outcome.absorbed),
            )
        })
}

/// What Mana Shield (60% of the largest loss) or Regrowth (35% of all losses)
/// recovers from the last day, before the daily generated-reward adjustment.
#[must_use]
pub fn mana_recovery_base(item_key: &str, largest_loss: i64, total_loss: i64) -> Option<i64> {
    let base = match item_key {
        "mana_shield" => (largest_loss as f64 * 0.60) as i64,
        "regrowth" => (total_loss as f64 * 0.35) as i64,
        _ => return None,
    };
    Some(base.min(MANA_RECOVERY_CAP))
}

#[cfg(test)]
#[path = "shop_commands/tests.rs"]
mod tests;
