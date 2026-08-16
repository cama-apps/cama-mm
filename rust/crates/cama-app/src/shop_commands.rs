//! Discord-independent shop command policy and atomic settlement seams.
//!
//! Python remains the live Discord entry point during the side-by-side rewrite.
//! This module keeps the command's validation order, visibility decisions,
//! pricing, and transaction boundaries independent from `discord.py`.  A live
//! adapter can implement the ports below without allowing Rust and Python to
//! write the same purchase twice.

use std::collections::HashMap;
use std::sync::Mutex;

use cama_db::package_deal_repository::calculate_package_deal_cost;
use cama_domain::economy_scaling::{
    DailyRewardPolicy, adjust_generated_jc_reward, scale_deflationary_minigame_jc_delta,
    scale_minigame_jc_delta,
};
use cama_domain::mana::ManaEffects;

pub const SHOP_ANNOUNCE_COST: i64 = 10;
pub const SHOP_ANNOUNCE_TARGET_COST: i64 = 100;
pub const SHOP_JOPA_COIN_COST: i64 = 10_000;
pub const SHOP_NEW_MYSTERY_GIFT_COST: i64 = 20_000;
pub const SHOP_DOUBLE_OR_NOTHING_COST: i64 = 50;
pub const SHOP_RECALIBRATE_COST: i64 = 300;
pub const SHOP_SOFT_AVOID_COST: i64 = 750;
pub const SOFT_AVOID_MIN_COST: i64 = 250;
pub const SOFT_AVOID_WINRATE_COST_SCALE: i64 = 1_000;
pub const SOFT_AVOID_MIN_TEAMMATE_GAMES: i64 = 3;
pub const SOFT_AVOID_GAMES: i64 = 10;
pub const PACKAGE_DEAL_MAX_GAMES: i64 = 10;
pub const PACKAGE_DEAL_NO_ACTIVE_COST: i64 = 1;
pub const PINGEDASH_COST: i64 = 10;
pub const PINGEDKEVIN_COST: i64 = PINGEDASH_COST;
pub const PINGEDASH_COOLDOWN_SECONDS: i64 = 86_400;
pub const PINGEDKEVIN_COOLDOWN_SECONDS: i64 = PINGEDASH_COOLDOWN_SECONDS;
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

    fn private_followup(content: impl Into<String>, embed_title: Option<&str>) -> Self {
        Self {
            content: content.into(),
            visibility: Visibility::Ephemeral,
            phase: ResponsePhase::FollowupAfterEphemeralDefer,
            embed_title: embed_title.map(str::to_owned),
            allowed_users: Vec::new(),
        }
    }
}

/// Persistence contract required before the Rust command can become live.
///
/// Every money-moving shop action is represented by one atomic operation.  A
/// Discord adapter must never synthesize these from read-then-write calls.
pub trait AtomicShopSettlementPort {
    type Error;

    fn settle_double_or_nothing_atomic(
        &self,
        request: DoubleOrNothingRequest,
    ) -> Result<DoubleOrNothingSettlement, Self::Error>;
    fn try_purchase_item_atomic(
        &self,
        request: ManaPurchaseRequest,
    ) -> Result<ManaPurchaseResult, Self::Error>;
    fn refund_item_purchase_atomic(&self, purchase_id: &str) -> Result<bool, Self::Error>;
    fn get_item_purchase(
        &self,
        purchase_id: &str,
    ) -> Result<Option<ManaPurchaseResult>, Self::Error>;
    fn mark_item_purchase_applying_atomic(&self, purchase_id: &str) -> Result<bool, Self::Error>;
    fn complete_item_purchase_atomic(&self, purchase_id: &str) -> Result<bool, Self::Error>;
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

pub const SHOP_BUY_PARAMETER_NAMES: &[&str] = &["interaction", "item", "target"];

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
    last_paid_ping: HashMap<PaidPingKind, i64>,
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

    #[must_use]
    pub fn last_paid_ping(
        &self,
        account_id: i64,
        guild_id: i64,
        kind: PaidPingKind,
    ) -> Option<i64> {
        self.state
            .lock()
            .expect("shop settlement mutex poisoned")
            .accounts
            .get(&(account_id, guild_id))
            .and_then(|account| account.last_paid_ping.get(&kind).copied())
    }

    #[must_use]
    pub fn try_purchase_paid_ping(
        &self,
        account_id: i64,
        guild_id: i64,
        kind: PaidPingKind,
        cost: i64,
        now: i64,
        cooldown_seconds: i64,
    ) -> PaidPingPurchase {
        let mut state = self.state.lock().expect("shop settlement mutex poisoned");
        let Some(account) = state.accounts.get_mut(&(account_id, guild_id)) else {
            return PaidPingPurchase {
                success: false,
                reason: Some(PurchaseFailureReason::NotRegistered),
                balance: 0,
                cooldown_ends_at: None,
            };
        };
        if let Some(last_used) = account.last_paid_ping.get(&kind).copied() {
            let cooldown_ends_at = last_used.saturating_add(cooldown_seconds);
            if now < cooldown_ends_at {
                return PaidPingPurchase {
                    success: false,
                    reason: Some(PurchaseFailureReason::OnCooldown),
                    balance: account.balance,
                    cooldown_ends_at: Some(cooldown_ends_at),
                };
            }
        }
        if account.balance < cost {
            return PaidPingPurchase {
                success: false,
                reason: Some(PurchaseFailureReason::InsufficientBalance),
                balance: account.balance,
                cooldown_ends_at: None,
            };
        }

        account.balance -= cost;
        account.last_paid_ping.insert(kind, now);
        let balance = account.balance;
        state.ledger.push(LedgerEntry {
            account_id,
            guild_id,
            delta: -cost,
            source: kind.command_name().to_owned(),
            related_type: "command".to_owned(),
            related_id: kind.command_name().to_owned(),
        });
        PaidPingPurchase {
            success: true,
            reason: None,
            balance,
            cooldown_ends_at: Some(now.saturating_add(cooldown_seconds)),
        }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedMention {
    pub everyone: bool,
    pub roles: bool,
    pub users: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaidPingDelivery {
    pub response: CommandResponse,
    pub allowed_mentions: AllowedMention,
}

#[must_use]
pub fn paid_ping_response(
    kind: PaidPingKind,
    target_user_id: Option<i64>,
    purchase: Option<&PaidPingPurchase>,
) -> CommandResponse {
    let Some(target_user_id) = target_user_id.filter(|target| *target > 0) else {
        return CommandResponse::initial_ephemeral(format!(
            "{} is unavailable because its target user is not configured.",
            kind.display_name()
        ));
    };
    let Some(purchase) = purchase else {
        return CommandResponse::initial_ephemeral(format!(
            "{} is unavailable right now.",
            kind.display_name()
        ));
    };
    if purchase.success {
        let mut response = CommandResponse::public_followup(
            format!("<@{target_user_id}>\n{}", kind.tenor_url()),
            None,
        );
        response.allowed_users.push(target_user_id);
        return response;
    }
    let message = match purchase.reason {
        Some(PurchaseFailureReason::NotRegistered) => format!(
            "You need to `/player register` before using `/shop {}`.",
            kind.command_name()
        ),
        Some(PurchaseFailureReason::OnCooldown) => format!(
            "{} is on cooldown. You can use it again <t:{}:R>.",
            kind.display_name(),
            purchase.cooldown_ends_at.unwrap_or_default()
        ),
        Some(PurchaseFailureReason::InsufficientBalance) => format!(
            "You need 10 jopacoin for {}, but you only have {}.",
            kind.display_name(),
            purchase.balance
        ),
        _ => format!("{} is unavailable right now.", kind.display_name()),
    };
    CommandResponse::initial_ephemeral(message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub name: &'static str,
    pub value: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedChoice {
    pub name: String,
    pub value: &'static str,
}

#[must_use]
pub fn item_autocomplete(
    current: &str,
    configured_package_games: i64,
    recalibration_on_cooldown: bool,
) -> Vec<OwnedChoice> {
    let package_games = package_deal_games(configured_package_games);
    let mut choices = vec![
        OwnedChoice {
            name: format!("Announce Balance ({SHOP_ANNOUNCE_COST} jopacoin)"),
            value: "announce",
        },
        OwnedChoice {
            name: format!("Announce Balance + Tag User ({SHOP_ANNOUNCE_TARGET_COST} jopacoin)"),
            value: "announce_target",
        },
        OwnedChoice {
            name: format!("Jopa Coin(TM) ({SHOP_JOPA_COIN_COST} jopacoin)"),
            value: "jopa_coin",
        },
        OwnedChoice {
            name: format!("Mystery Gift ({SHOP_NEW_MYSTERY_GIFT_COST} jopacoin)"),
            value: "mystery_gift",
        },
        OwnedChoice {
            name: format!("Double or Nothing ({SHOP_DOUBLE_OR_NOTHING_COST} jopacoin)"),
            value: "double_or_nothing",
        },
        OwnedChoice {
            name: format!(
                "Soft Avoid (dynamic, minimum {SOFT_AVOID_MIN_COST}, default {SHOP_SOFT_AVOID_COST} jopacoin for {SOFT_AVOID_GAMES} games)"
            ),
            value: "soft_avoid",
        },
        OwnedChoice {
            name: format!(
                "Package Deal ({PACKAGE_DEAL_NO_ACTIVE_COST} JC at 0 active, 500+ after for {package_games} games)"
            ),
            value: "package_deal",
        },
        if recalibration_on_cooldown {
            OwnedChoice {
                name: "Recalibrate (ON COOLDOWN)".to_owned(),
                value: "recalibrate_cooldown",
            }
        } else {
            OwnedChoice {
                name: format!("Recalibrate ({SHOP_RECALIBRATE_COST} jopacoin)"),
                value: "recalibrate",
            }
        },
    ];
    if !current.is_empty() {
        let current = current.to_lowercase();
        choices.retain(|choice| choice.name.to_lowercase().contains(&current));
    }
    choices.truncate(25);
    choices
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShopRoute {
    Announce,
    AnnounceTarget(i64),
    JopaCoin,
    MysteryGift,
    DoubleOrNothing,
    SoftAvoid(i64),
    PackageDeal(i64),
    Recalibrate,
    RecalibrateCooldown,
}

pub fn route_shop_buy(item: &str, target: Option<i64>) -> Result<ShopRoute, CommandResponse> {
    match (item, target) {
        ("announce", _) => Ok(ShopRoute::Announce),
        ("announce_target", Some(target)) => Ok(ShopRoute::AnnounceTarget(target)),
        ("announce_target", None) => Err(CommandResponse::initial_ephemeral(
            "You selected 'Announce + Tag User' but didn't specify a target. Please provide a user to tag!",
        )),
        ("jopa_coin", _) => Ok(ShopRoute::JopaCoin),
        ("mystery_gift", _) => Ok(ShopRoute::MysteryGift),
        ("double_or_nothing", _) => Ok(ShopRoute::DoubleOrNothing),
        ("soft_avoid", Some(target)) => Ok(ShopRoute::SoftAvoid(target)),
        ("soft_avoid", None) => Err(CommandResponse::initial_ephemeral(
            "You selected 'Soft Avoid' but didn't specify a target.",
        )),
        ("package_deal", Some(target)) => Ok(ShopRoute::PackageDeal(target)),
        ("package_deal", None) => Err(CommandResponse::initial_ephemeral(
            "You selected 'Package Deal' but didn't specify a target.",
        )),
        ("recalibrate", _) => Ok(ShopRoute::Recalibrate),
        ("recalibrate_cooldown", _) => Ok(ShopRoute::RecalibrateCooldown),
        _ => Err(CommandResponse::initial_ephemeral(
            "That shop item is no longer available.",
        )),
    }
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
            Self::MysteryGift => "Mystery Gift Redeemed!",
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecalibrationOutcome {
    pub purchase: SimplePurchaseOutcome,
    pub recalibrated: bool,
    pub refund: Option<SpendIntent>,
}

#[must_use]
pub fn recalibration_purchase(
    user_id: i64,
    eligibility: RecalibrationEligibility,
    balance: i64,
    debit_committed: bool,
    recalibration_succeeded: bool,
    notice_transport_alive: bool,
) -> RecalibrationOutcome {
    if eligibility != RecalibrationEligibility::Allowed {
        let message = match eligibility {
            RecalibrationEligibility::NotRegistered => {
                "You need to `/player register` before you can recalibrate.".to_owned()
            }
            RecalibrationEligibility::NoRating => {
                "You don't have a rating yet. Play some games first!".to_owned()
            }
            RecalibrationEligibility::InsufficientGames {
                games_played,
                min_games,
            } => format!(
                "You need at least {min_games} games to recalibrate (you have {games_played})."
            ),
            RecalibrationEligibility::OnCooldown { ends_at } => {
                format!("Recalibration is on cooldown. You can recalibrate again <t:{ends_at}:R>.")
            }
            RecalibrationEligibility::Allowed => unreachable!(),
        };
        return RecalibrationOutcome {
            purchase: SimplePurchaseOutcome {
                response: CommandResponse::initial_ephemeral(message),
                spend: None,
                delivered: false,
                notice_transport_lapsed: !notice_transport_alive,
            },
            recalibrated: false,
            refund: None,
        };
    }

    let purchase = simple_purchase(
        SimpleProduct::Recalibrate,
        user_id,
        true,
        balance,
        debit_committed,
        notice_transport_alive,
    );
    if !purchase.delivered {
        return RecalibrationOutcome {
            purchase,
            recalibrated: false,
            refund: None,
        };
    }
    let refund = (!recalibration_succeeded).then_some(SpendIntent {
        amount: SHOP_RECALIBRATE_COST,
        source: "shop_recalibrate",
        actor_id: user_id,
    });
    RecalibrationOutcome {
        purchase,
        recalibrated: recalibration_succeeded,
        refund,
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftAvoidFailure {
    AlreadyActive { games_remaining: i64 },
    InsufficientBalance { balance: i64 },
    Storage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoftAvoidCommandOutcome {
    pub cost: i64,
    pub response: CommandResponse,
    pub charged_atomically: bool,
}

#[must_use]
pub fn soft_avoid_command_outcome(
    target_name: &str,
    pairing: Option<PairingRecord>,
    preexisting_games: Option<i64>,
    purchase: Result<i64, SoftAvoidFailure>,
) -> SoftAvoidCommandOutcome {
    let cost = calculate_soft_avoid_cost(pairing, SHOP_SOFT_AVOID_COST);
    if let Some(games) = preexisting_games {
        return SoftAvoidCommandOutcome {
            cost,
            response: CommandResponse::initial_ephemeral(format!(
                "Your soft avoid for **{target_name}** is already active with {games} games remaining."
            )),
            charged_atomically: false,
        };
    }
    match purchase {
        Ok(games_remaining) => SoftAvoidCommandOutcome {
            cost,
            response: CommandResponse::private_followup(
                format!("Soft Avoid Active: {games_remaining} games remaining"),
                Some("Soft Avoid Active"),
            ),
            charged_atomically: true,
        },
        Err(SoftAvoidFailure::AlreadyActive { games_remaining }) => SoftAvoidCommandOutcome {
            cost,
            response: CommandResponse::private_followup(
                format!(
                    "Your soft avoid for **{target_name}** is already active with {games_remaining} games remaining."
                ),
                None,
            ),
            charged_atomically: false,
        },
        Err(SoftAvoidFailure::InsufficientBalance { balance }) => SoftAvoidCommandOutcome {
            cost,
            response: CommandResponse::private_followup(
                format!("You need {cost} jopacoin for this, but you only have {balance}."),
                None,
            ),
            charged_atomically: false,
        },
        Err(SoftAvoidFailure::Storage) => SoftAvoidCommandOutcome {
            cost,
            response: CommandResponse::private_followup(
                "Soft avoid purchase failed before it could be activated. You were not charged.",
                None,
            ),
            charged_atomically: false,
        },
    }
}

#[must_use]
pub fn package_deal_games(configured_duration: i64) -> i64 {
    configured_duration.min(PACKAGE_DEAL_MAX_GAMES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageDealFailure {
    AlreadyFull { games_remaining: i64 },
    InsufficientBalance { balance: i64 },
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageDealSuccess {
    pub cost: i64,
    pub active_deal_count: i64,
    pub games_added: i64,
    pub games_remaining: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDealCommandOutcome {
    pub active_cost: i64,
    pub requested_games: i64,
    pub response: CommandResponse,
    pub charged_atomically: bool,
}

#[must_use]
pub fn package_deal_command_outcome(
    target_name: &str,
    buyer_rating: Option<f64>,
    target_rating: Option<f64>,
    configured_duration: i64,
    purchase: Result<PackageDealSuccess, PackageDealFailure>,
) -> PackageDealCommandOutcome {
    let active_cost = calculate_package_deal_cost(1, false, buyer_rating, target_rating);
    let requested_games = package_deal_games(configured_duration);
    let (response, charged_atomically) = match purchase {
        Ok(success) => (
            CommandResponse::private_followup(
                format!(
                    "Package Deal Active\n**Games added:** {}\n**Games remaining:** {}\n**Cost:** {} jopacoin",
                    success.games_added, success.games_remaining, success.cost
                ),
                Some("Package Deal Active"),
            ),
            true,
        ),
        Err(PackageDealFailure::AlreadyFull { games_remaining }) => (
            CommandResponse::private_followup(
                format!(
                    "Your Package Deal with **{target_name}** already has {games_remaining} games remaining. You were not charged."
                ),
                None,
            ),
            false,
        ),
        Err(PackageDealFailure::InsufficientBalance { balance }) => (
            CommandResponse::private_followup(
                format!("You need {active_cost} jopacoin for this, but you only have {balance}."),
                None,
            ),
            false,
        ),
        Err(PackageDealFailure::Storage) => (
            CommandResponse::private_followup(
                "Package deal purchase failed before it could be activated. You were not charged.",
                None,
            ),
            false,
        ),
    };
    PackageDealCommandOutcome {
        active_cost,
        requested_games,
        response,
        charged_atomically,
    }
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
pub struct ManaPurchaseRequest {
    pub account_id: i64,
    pub guild_id: i64,
    pub item_key: &'static str,
    pub day_key: &'static str,
    pub cost: i64,
    pub tap_mana: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManaPurchaseFailure {
    AlreadyUsed,
    PurchaseInProgress,
    ManaTapped,
    NotRegistered,
    InsufficientBalance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManaPurchaseResult {
    pub success: bool,
    pub reason: Option<ManaPurchaseFailure>,
    pub balance: i64,
    pub purchase_id: Option<String>,
    pub item_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManaValidation {
    pub response: Option<CommandResponse>,
    pub request: Option<ManaPurchaseRequest>,
    pub defer_public: bool,
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn validate_mana_purchase(
    user_id: i64,
    guild_id: i64,
    registered: bool,
    mana_system_available: bool,
    tapped: bool,
    effects: &ManaEffects,
    item_key: &'static str,
    target_id: Option<i64>,
    observed_balance: i64,
    atomic_result: Option<&ManaPurchaseResult>,
) -> ManaValidation {
    let reject = |message: String| ManaValidation {
        response: Some(CommandResponse::initial_ephemeral(message)),
        request: None,
        defer_public: false,
    };
    if !registered {
        return reject("You need to `/player register` first.".to_owned());
    }
    if !mana_system_available {
        return reject("Mana system not available.".to_owned());
    }
    if tapped {
        return reject("Your mana is spent — come back tomorrow.".to_owned());
    }
    let Some(color) = effects.color.as_deref() else {
        return reject("You have no active mana today. Use `/mana` first.".to_owned());
    };
    let Some(spec) = mana_item(item_key) else {
        return reject("Unknown item.".to_owned());
    };
    if color != spec.color {
        return reject(format!(
            "**{}** requires **{}** mana. Your current mana is **{color}**.",
            spec.name, spec.color
        ));
    }
    if spec.target_required && target_id.is_none() {
        return reject(format!("**{}** requires a `target` player.", spec.name));
    }
    if !spec.self_target_allowed && target_id == Some(user_id) {
        return reject(format!("**{}** must target another player.", spec.name));
    }
    if observed_balance < spec.cost {
        return reject(format!(
            "You need {} jopacoin for {}. You have {observed_balance}.",
            spec.cost, spec.name
        ));
    }
    let request = ManaPurchaseRequest {
        account_id: user_id,
        guild_id,
        item_key: spec.key,
        day_key: "today-pst",
        cost: spec.cost,
        tap_mana: spec.tier == ManaTier::Ultimate,
    };
    if let Some(result) = atomic_result
        && !result.success
    {
        let message = match result.reason {
            Some(ManaPurchaseFailure::AlreadyUsed) => {
                format!("**{}** is once-per-day. Already used today.", spec.name)
            }
            Some(ManaPurchaseFailure::PurchaseInProgress) => format!(
                "Your previous **{}** purchase is still starting. Try again shortly.",
                spec.name
            ),
            Some(ManaPurchaseFailure::ManaTapped) => {
                "Your mana was already tapped this turn. Try again tomorrow.".to_owned()
            }
            Some(ManaPurchaseFailure::NotRegistered) => {
                "You need to `/player register` first.".to_owned()
            }
            _ => format!(
                "You need {} jopacoin for {}. You have {}.",
                spec.cost, spec.name, result.balance
            ),
        };
        return ManaValidation {
            response: Some(CommandResponse::initial_ephemeral(message)),
            request: Some(request),
            defer_public: false,
        };
    }
    ManaValidation {
        response: None,
        request: Some(request),
        defer_public: true,
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeneratedRecovery {
    pub base_reward: i64,
    pub adjusted_reward: i64,
}

#[must_use]
pub fn generated_recovery(
    loss: i64,
    rate: f64,
    cap: i64,
    guild_id: Option<i64>,
    daily_reward_policy: Option<&mut dyn DailyRewardPolicy>,
) -> GeneratedRecovery {
    let base_reward = ((loss as f64 * rate) as i64).min(cap);
    let adjusted_reward =
        adjust_generated_jc_reward(base_reward as f64, 1.0, guild_id, daily_reward_policy);
    GeneratedRecovery {
        base_reward,
        adjusted_reward,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostileCandidate {
    pub account_id: i64,
    pub name: String,
    pub balance: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostileLossRequest {
    pub victim_id: i64,
    pub guild_id: i64,
    pub amount: i64,
    pub kind: &'static str,
    pub event_key: String,
    pub destination: &'static str,
    pub recipient_id: Option<i64>,
    pub clamp_to_balance: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtectionOutcome {
    pub attempted_loss: i64,
    pub absorbed_amount: i64,
    pub applied_loss: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostileSummary {
    pub applied_loss: i64,
    pub absorbed_amount: i64,
    pub reward: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostilePurchaseDisposition {
    Apply,
    RefundAndReleaseDailySlot,
}

#[must_use]
pub fn hostile_purchase_disposition(requests: &[HostileLossRequest]) -> HostilePurchaseDisposition {
    if requests.is_empty() {
        HostilePurchaseDisposition::RefundAndReleaseDailySlot
    } else {
        HostilePurchaseDisposition::Apply
    }
}

fn eligible_hostile_targets(
    buyer_id: i64,
    candidates: &[HostileCandidate],
) -> impl Iterator<Item = &HostileCandidate> {
    candidates
        .iter()
        .filter(move |candidate| candidate.account_id != buyer_id)
        .filter(|candidate| candidate.balance >= HOSTILE_LOSS_MIN_BALANCE)
}

#[must_use]
pub fn pyroclasm_requests(
    buyer_id: i64,
    guild_id: i64,
    candidates: &[HostileCandidate],
    rolls: &[i64],
    purchase_id: &str,
) -> Vec<HostileLossRequest> {
    eligible_hostile_targets(buyer_id, candidates)
        .take(3)
        .zip(rolls.iter().copied())
        .filter_map(|(candidate, roll)| {
            let amount =
                scale_deflationary_minigame_jc_delta(roll as f64, 1.0).min(candidate.balance);
            (amount > 0).then(|| HostileLossRequest {
                victim_id: candidate.account_id,
                guild_id,
                amount,
                kind: "pyroclasm",
                event_key: format!("pyroclasm:{purchase_id}:{}", candidate.account_id),
                destination: "burn",
                recipient_id: None,
                clamp_to_balance: true,
            })
        })
        .collect()
}

#[must_use]
pub fn settle_pyroclasm(outcomes: &[ProtectionOutcome]) -> HostileSummary {
    let applied_loss = outcomes.iter().map(|outcome| outcome.applied_loss).sum();
    let absorbed_amount = outcomes.iter().map(|outcome| outcome.absorbed_amount).sum();
    HostileSummary {
        applied_loss,
        absorbed_amount,
        reward: (applied_loss / 2).min(35),
    }
}

#[must_use]
pub fn soul_harvest_requests(
    buyer_id: i64,
    guild_id: i64,
    candidates: &[HostileCandidate],
    bonus_rolls: &[f64],
    purchase_id: &str,
) -> Vec<HostileLossRequest> {
    let base = scale_minigame_jc_delta(2.0, 1.0);
    eligible_hostile_targets(buyer_id, candidates)
        .zip(bonus_rolls.iter().copied())
        .map(|(candidate, bonus_roll)| {
            let amount = (base + i64::from(bonus_roll < 0.20)).min(candidate.balance);
            HostileLossRequest {
                victim_id: candidate.account_id,
                guild_id,
                amount,
                kind: "soul_harvest",
                event_key: format!("soul_harvest:{purchase_id}:{}", candidate.account_id),
                destination: "player",
                recipient_id: Some(buyer_id),
                clamp_to_balance: true,
            }
        })
        .collect()
}

#[must_use]
pub fn settle_soul_harvest(outcomes: &[ProtectionOutcome]) -> HostileSummary {
    let applied_loss = outcomes.iter().map(|outcome| outcome.applied_loss).sum();
    let absorbed_amount = outcomes.iter().map(|outcome| outcome.absorbed_amount).sum();
    HostileSummary {
        applied_loss,
        absorbed_amount,
        reward: applied_loss,
    }
}

#[must_use]
pub fn wildfire_requests(
    buyer_id: i64,
    guild_id: i64,
    candidates: &[HostileCandidate],
    rolls: &[i64],
    purchase_id: &str,
) -> Vec<HostileLossRequest> {
    eligible_hostile_targets(buyer_id, candidates)
        .zip(rolls.iter().copied())
        .filter_map(|(candidate, roll)| {
            let amount =
                scale_deflationary_minigame_jc_delta(roll as f64, 1.0).min(candidate.balance);
            (amount > 0).then(|| HostileLossRequest {
                victim_id: candidate.account_id,
                guild_id,
                amount,
                kind: "wildfire",
                event_key: format!("wildfire:{purchase_id}:{}", candidate.account_id),
                destination: "burn",
                recipient_id: None,
                clamp_to_balance: true,
            })
        })
        .collect()
}

#[must_use]
pub fn settle_wildfire(outcomes: &[ProtectionOutcome]) -> HostileSummary {
    let applied_loss = outcomes.iter().map(|outcome| outcome.applied_loss).sum();
    let absorbed_amount = outcomes.iter().map(|outcome| outcome.absorbed_amount).sum();
    HostileSummary {
        applied_loss,
        absorbed_amount,
        reward: (applied_loss as f64 * 0.45) as i64,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReprieveEffect {
    pub pool: i64,
    pub recovered: i64,
    pub lookback_seconds: i64,
}

#[must_use]
pub const fn reprieve_effect(recovered: i64) -> ReprieveEffect {
    ReprieveEffect {
        pool: 25,
        recovered,
        lookback_seconds: 24 * 3_600,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SanctuaryEffect {
    pub cost: i64,
    pub shared_pool: i64,
    pub match_bonus: i64,
}

#[must_use]
pub const fn sanctuary_effect() -> SanctuaryEffect {
    SanctuaryEffect {
        cost: 90,
        shared_pool: 150,
        match_bonus: 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DarkBargainTerms {
    pub cost: i64,
    pub principal: i64,
    pub amount_due: i64,
    pub due_in_days: i64,
}

#[must_use]
pub const fn dark_bargain_terms() -> DarkBargainTerms {
    DarkBargainTerms {
        cost: 150,
        principal: 800,
        amount_due: 700,
        due_in_days: 7,
    }
}

#[cfg(test)]
mod tests;
