//! Discord-independent command and delivery orchestration for Camagotchi pets.
//!
//! Python currently owns the live gateway adapter.  This module preserves its
//! acknowledgement, visibility, rendering, retry, button, and reminder rules
//! behind typed ports.  The service port deliberately consumes the existing
//! `cama-app::pet` outcome types and `cama-domain::pet` models; a future live
//! adapter only has to translate Discord objects and the few command-only
//! service operations that have not yet moved into the Rust core service.

use std::collections::{BTreeMap, BTreeSet};

use cama_domain::formatting::JOPACOIN_EMOTE;
use cama_domain::pet::{
    ACCESSORIES, DIG_WORK_CAP_BLOCKS, DIG_WORK_UNITS_PER_BLOCK, FEED_CAP_PER_DAY, FOOD_ITEMS,
    FeedOutcome, GILDED_EGG_PREMIUM, Pet, PetMood, PetStage, PetStatus, RENAME_COST, RefundNotice,
    SALT_LICK, SOLO_TRAINING_SESSION_CAP, SPECIES, SpeciesTier, TrinketOutcome, UNHATCHED_SPECIES,
    WARNING_HUNGER, food_cost, get_accessory, get_species,
};
use cama_domain::pet_evolution::{
    PetCalling, PetInstinct, calling_profile, hint_text, instinct_label,
};
use cama_domain::service_result::ServiceResult;

use crate::pet::{AdoptOutcome, BuyOutcome, SacrificeOutcome, SacrificePreview, SweepOutcome};

pub const COMMAND_GROUP: CommandGroup = CommandGroup {
    name: "pet",
    description: "Adopt and care for your cama (camel-llama hybrid)",
    subcommands: &[
        "adopt",
        "status",
        "train",
        "feed",
        "shop",
        "buy",
        "rename",
        "trinket",
        "brawl",
        "altar",
        "eat",
        "graveyard",
        "leaderboard",
    ],
};

const DAY_SECONDS: i64 = 86_400;
const STATUS_VIEW_TIMEOUT_SECONDS: u64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandGroup {
    pub name: &'static str,
    pub description: &'static str,
    pub subcommands: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    #[default]
    Public,
    Ephemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedColor {
    Blue,
    Green,
    Gold,
    Orange,
    Red,
    Slate,
    Custom(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub color: EmbedColor,
    pub fields: Vec<EmbedField>,
    pub image: Option<String>,
    pub footer: Option<String>,
}

impl Embed {
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        color: EmbedColor,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            color,
            fields: Vec::new(),
            image: None,
            footer: None,
        }
    }

    pub fn field(&mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline,
        });
    }

    #[must_use]
    pub fn field_value(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusAction {
    Feed(String),
    Buy(String),
    SaltLick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusButton {
    pub label: String,
    pub disabled: bool,
    pub action: StatusAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusView {
    pub owner_id: i64,
    pub guild_id: Option<i64>,
    pub timeout_seconds: u64,
    pub buttons: Vec<StatusButton>,
}

impl StatusView {
    #[must_use]
    pub fn new(
        owner_id: i64,
        guild_id: Option<i64>,
        supplies: Option<&BTreeMap<String, i64>>,
        species_id: &str,
        can_feed: bool,
    ) -> Self {
        let empty = BTreeMap::new();
        let supplies = supplies.unwrap_or(&empty);
        let mut buttons = Vec::with_capacity(FOOD_ITEMS.len() * 2 + 1);
        for food in FOOD_ITEMS {
            let owned = supplies.get(food.item_id).copied().unwrap_or_default();
            buttons.push(StatusButton {
                label: format!("Feed {} ×{owned}", food.display_name),
                disabled: !can_feed || owned <= 0,
                action: StatusAction::Feed(food.item_id.to_owned()),
            });
        }
        for food in FOOD_ITEMS {
            buttons.push(StatusButton {
                label: format!(
                    "Buy {} ({})",
                    food.display_name,
                    food_cost(species_id, food.item_id).unwrap_or(food.cost)
                ),
                disabled: false,
                action: StatusAction::Buy(food.item_id.to_owned()),
            });
        }
        buttons.push(StatusButton {
            label: format!(
                "Salt Lick ({})",
                food_cost(species_id, SALT_LICK.item_id).unwrap_or(SALT_LICK.cost)
            ),
            disabled: false,
            action: StatusAction::SaltLick,
        });
        Self {
            owner_id,
            guild_id,
            timeout_seconds: STATUS_VIEW_TIMEOUT_SECONDS,
            buttons,
        }
    }

    pub fn interaction_check(&self, user_id: i64) -> Result<(), Box<ResponsePayload>> {
        if user_id == self.owner_id {
            return Ok(());
        }
        Err(Box::new(ResponsePayload::content(
            "That's not your cama. Adopt your own with `/pet adopt`!",
            Visibility::Ephemeral,
        )))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResponsePayload {
    pub content: Option<String>,
    pub embed: Option<Embed>,
    pub attachment: Option<Attachment>,
    pub view: Option<StatusView>,
    pub visibility: Visibility,
}

impl ResponsePayload {
    #[must_use]
    pub fn content(content: impl Into<String>, visibility: Visibility) -> Self {
        Self {
            content: Some(content.into()),
            visibility,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn embed(embed: Embed, visibility: Visibility) -> Self {
        Self {
            embed: Some(embed),
            visibility,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteractionEvent {
    Immediate(ResponsePayload),
    Deferred(Visibility),
    Followup(ResponsePayload),
    EditOriginal(ResponsePayload),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub message: String,
}

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub trait InteractionPort {
    fn send_immediate(&mut self, payload: ResponsePayload) -> Result<(), TransportError>;
    fn defer(&mut self, visibility: Visibility) -> bool;
    fn send_followup(&mut self, payload: ResponsePayload) -> Result<(), TransportError>;
    fn edit_original(&mut self, payload: ResponsePayload) -> Result<(), TransportError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionRecorder {
    pub events: Vec<InteractionEvent>,
    pub allow_defer: bool,
}

impl Default for InteractionRecorder {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            allow_defer: true,
        }
    }
}

impl InteractionRecorder {
    #[must_use]
    pub fn last_payload(&self) -> Option<&ResponsePayload> {
        self.events.iter().rev().find_map(|event| match event {
            InteractionEvent::Immediate(payload)
            | InteractionEvent::Followup(payload)
            | InteractionEvent::EditOriginal(payload) => Some(payload),
            InteractionEvent::Deferred(_) => None,
        })
    }
}

impl InteractionPort for InteractionRecorder {
    fn send_immediate(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
        self.events.push(InteractionEvent::Immediate(payload));
        Ok(())
    }

    fn defer(&mut self, visibility: Visibility) -> bool {
        if self.allow_defer {
            self.events.push(InteractionEvent::Deferred(visibility));
        }
        self.allow_defer
    }

    fn send_followup(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
        self.events.push(InteractionEvent::Followup(payload));
        Ok(())
    }

    fn edit_original(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
        self.events.push(InteractionEvent::EditOriginal(payload));
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionContext {
    pub user_id: i64,
    pub display_name: String,
    pub guild_id: Option<i64>,
    pub now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PetFlavorEvent {
    Adopted,
    Status,
    Fed,
    Spat,
    Hatched,
    Evolved,
    Died,
}

pub trait PetFlavorPort {
    fn generate(
        &mut self,
        event: PetFlavorEvent,
        pet: &Pet,
        status: Option<&PetStatus>,
    ) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoPetFlavor;

impl PetFlavorPort for NoPetFlavor {
    fn generate(
        &mut self,
        _event: PetFlavorEvent,
        _pet: &Pet,
        _status: Option<&PetStatus>,
    ) -> Option<String> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub retry_after_seconds: i64,
}

pub trait RateLimitPort {
    fn check(&mut self, scope: &str, guild_id: i64, user_id: i64) -> RateLimitDecision;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl RateLimitPort for AllowAll {
    fn check(&mut self, _scope: &str, _guild_id: i64, _user_id: i64) -> RateLimitDecision {
        RateLimitDecision {
            allowed: true,
            retry_after_seconds: 0,
        }
    }
}

pub trait PetReminderPort {
    fn pet_enabled(&mut self, discord_id: i64, guild_id: i64) -> bool;
    fn schedule_pet_reminder(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        crossing: i64,
        pet_name: &str,
        preference_enabled: bool,
    );
    fn cancel_pet_reminder(&mut self, discord_id: i64, guild_id: i64);
}

#[derive(Clone, Debug, Default)]
pub struct NoPetReminders;

impl PetReminderPort for NoPetReminders {
    fn pet_enabled(&mut self, _discord_id: i64, _guild_id: i64) -> bool {
        false
    }

    fn schedule_pet_reminder(
        &mut self,
        _discord_id: i64,
        _guild_id: i64,
        _crossing: i64,
        _pet_name: &str,
        _preference_enabled: bool,
    ) {
    }

    fn cancel_pet_reminder(&mut self, _discord_id: i64, _guild_id: i64) {}
}

/// Command-facing contract.  Return values are shared with the already ported
/// pet application service, while command-only reads/writes remain explicit so
/// a live adapter cannot silently invent behavior.
pub trait PetCommandService {
    fn decay_per_day(&self) -> i64;
    fn sweep(&mut self) -> Result<SweepOutcome, String>;
    fn mark_hatch_announced(&mut self, pet: &Pet) -> Result<(), String>;
    fn mark_evolution_announced(&mut self, pet: &Pet) -> Result<(), String>;
    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String>;
    fn mark_refund_announced(&mut self, notice: &RefundNotice) -> Result<(), String>;
    fn adopt(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
        egg_tier: &str,
    ) -> ServiceResult<AdoptOutcome>;
    fn stable(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Vec<Pet>>;
    fn activate(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        pet_id: i64,
    ) -> ServiceResult<cama_db::pet_repository::ActivatePetOutcome>;
    fn upgrade(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        pet_id: i64,
    ) -> ServiceResult<Pet>;
    fn sacrifice_preview(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SacrificePreview>;
    fn sacrifice(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
    ) -> ServiceResult<SacrificeOutcome>;
    fn status(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<PetStatus>;
    fn next_adoption_fee(&mut self, discord_id: i64, guild_id: Option<i64>) -> i64;
    fn balance(&mut self, discord_id: i64, guild_id: Option<i64>) -> Option<i64>;
    fn buy(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
        quantity: i64,
    ) -> ServiceResult<BuyOutcome>;
    fn feed(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
    ) -> ServiceResult<FeedOutcome>;
    fn roll_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<TrinketOutcome>;
    fn wear_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: &str,
    ) -> ServiceResult<String>;
    fn owned_trinkets(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<String>;
    fn rename(&mut self, discord_id: i64, guild_id: Option<i64>, name: &str) -> ServiceResult<Pet>;
    fn graveyard(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<Pet>;
    fn leaderboard(&mut self, guild_id: Option<i64>) -> Vec<Pet>;
    fn warning_crossing_for(&self, pet: &Pet) -> Result<i64, String> {
        pet.hunger_crossing_time(WARNING_HUNGER, self.decay_per_day())
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryError {
    Forbidden,
    Transient,
}

pub trait DiscordDeliveryPort {
    fn resolve_text_channel(&mut self, guild_id: i64, channel_id: i64) -> Option<i64>;
    fn send_channel(
        &mut self,
        channel_id: i64,
        payload: ResponsePayload,
    ) -> Result<(), DeliveryError>;
    fn send_dm(&mut self, discord_id: i64, payload: ResponsePayload) -> Result<(), DeliveryError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetCommandConfig {
    pub pet_channel_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    MissingBrawlService,
}

pub fn setup_decision(
    pet_service_present: bool,
    pet_brawl_service_present: bool,
) -> Result<bool, SetupError> {
    if !pet_service_present {
        return Ok(false);
    }
    if !pet_brawl_service_present {
        return Err(SetupError::MissingBrawlService);
    }
    Ok(true)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PetFeatureComponents {
    pub pet_service: bool,
    pub evolution_service: bool,
    pub flavor_service: bool,
    pub flavor_ai_service: bool,
    pub shares_evolution_service: bool,
    pub shares_guild_config_repo: bool,
    pub shares_economy_ledger_repo: bool,
    pub shares_player_repo: bool,
}

impl PetFeatureComponents {
    #[must_use]
    pub const fn for_channel(channel_id: Option<i64>) -> Self {
        if channel_id.is_none() {
            return Self {
                pet_service: false,
                evolution_service: false,
                flavor_service: false,
                flavor_ai_service: false,
                shares_evolution_service: false,
                shares_guild_config_repo: false,
                shares_economy_ledger_repo: false,
                shares_player_repo: false,
            };
        }
        Self {
            pet_service: true,
            evolution_service: true,
            flavor_service: true,
            flavor_ai_service: false,
            shares_evolution_service: true,
            shares_guild_config_repo: true,
            shares_economy_ledger_repo: true,
            shares_player_repo: true,
        }
    }
}

pub struct PetCommands<S, F, M, L> {
    service: S,
    flavor: F,
    reminders: Option<M>,
    rate_limiter: L,
    config: PetCommandConfig,
    direct_death_deliveries: BTreeMap<i64, usize>,
}

impl<S, F, M, L> PetCommands<S, F, M, L>
where
    S: PetCommandService,
    F: PetFlavorPort,
    M: PetReminderPort,
    L: RateLimitPort,
{
    #[must_use]
    pub fn new(
        service: S,
        flavor: F,
        reminders: Option<M>,
        rate_limiter: L,
        config: PetCommandConfig,
    ) -> Self {
        Self {
            service,
            flavor,
            reminders,
            rate_limiter,
            config,
            direct_death_deliveries: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn service(&self) -> &S {
        &self.service
    }

    pub const fn service_mut(&mut self) -> &mut S {
        &mut self.service
    }

    #[must_use]
    pub const fn flavor(&self) -> &F {
        &self.flavor
    }

    #[must_use]
    pub const fn reminders(&self) -> Option<&M> {
        self.reminders.as_ref()
    }

    pub const fn reminders_mut(&mut self) -> Option<&mut M> {
        self.reminders.as_mut()
    }

    pub fn set_direct_death_delivery(&mut self, pet_id: i64, count: usize) {
        if count == 0 {
            self.direct_death_deliveries.remove(&pet_id);
        } else {
            self.direct_death_deliveries.insert(pet_id, count);
        }
    }

    pub fn adopt<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        name: &str,
        egg_tier: &str,
    ) {
        if !self.check_rate_limit(interaction, context, "pet") {
            return;
        }
        if !interaction.defer(Visibility::Public) {
            return;
        }
        match self
            .service
            .adopt(context.user_id, context.guild_id, name, egg_tier)
        {
            ServiceResult::Failure { error, .. } => {
                let _ = interaction.send_followup(error_payload(error));
            }
            ServiceResult::Success(outcome) => {
                let mut embed = build_adoption_embed(&outcome, &context.display_name);
                if let Some(flavor) =
                    self.flavor
                        .generate(PetFlavorEvent::Adopted, &outcome.pet, None)
                {
                    embed.field("💬 Cama chatter", flavor, false);
                }
                let payload = ResponsePayload {
                    embed: Some(embed),
                    attachment: Some(egg_attachment(outcome.pet.pet_id)),
                    ..ResponsePayload::default()
                };
                let _ = interaction.send_followup(payload);
            }
        }
    }

    pub fn status<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        target_id: i64,
        target_name: &str,
        public: bool,
    ) {
        if !self.check_rate_limit(interaction, context, "pet_status") {
            return;
        }
        let visibility = if public {
            Visibility::Public
        } else {
            Visibility::Ephemeral
        };
        if !interaction.defer(visibility) {
            return;
        }
        let with_view = target_id == context.user_id;
        let payload = self.compose_status(
            target_id,
            context.guild_id,
            target_name,
            with_view,
            PetFlavorEvent::Status,
            visibility,
            context.now,
        );
        let _ = interaction.send_followup(payload);
    }

    pub fn buy<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        item_id: &str,
        quantity: i64,
    ) {
        if !interaction.defer(Visibility::Ephemeral) {
            return;
        }
        match self
            .service
            .buy(context.user_id, context.guild_id, item_id, quantity)
        {
            ServiceResult::Failure { error, .. } => {
                let _ = interaction.send_followup(error_payload(error));
            }
            ServiceResult::Success(outcome) => {
                let content = if item_id == SALT_LICK.item_id {
                    let pet = outcome.pet.as_ref();
                    format!(
                        "🧂 **{}** is thoroughly pampered until <t:{}:t> (-{} {JOPACOIN_EMOTE})",
                        pet.map_or("Your cama", |candidate| candidate.name.as_str()),
                        outcome.pampered_until.unwrap_or_default(),
                        outcome.total_cost
                    )
                } else {
                    let food = food_by_id(item_id);
                    format!(
                        "{} Bought {}× {} for {} {JOPACOIN_EMOTE} — you now have ×{}.",
                        food.map_or("🎒", |item| item.emoji),
                        outcome.qty,
                        food.map_or(item_id, |item| item.display_name),
                        outcome.total_cost,
                        outcome.new_qty.unwrap_or_default()
                    )
                };
                let _ = interaction
                    .send_followup(ResponsePayload::content(content, Visibility::Ephemeral));
            }
        }
    }

    pub fn feed<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        item_id: &str,
    ) {
        if !interaction.defer(Visibility::Ephemeral) {
            return;
        }
        let result = self
            .service
            .feed(context.user_id, context.guild_id, item_id);
        let ServiceResult::Success(outcome) = result else {
            let error = result.error().unwrap_or("Pet feed failed.");
            let _ = interaction.send_followup(error_payload(error));
            return;
        };
        let status = self
            .service
            .status(context.user_id, context.guild_id)
            .value()
            .cloned();
        let event = if outcome.spat {
            PetFlavorEvent::Spat
        } else {
            PetFlavorEvent::Fed
        };
        let flavor = self.flavor.generate(event, &outcome.pet, status.as_ref());
        let food = food_by_id(item_id);
        let flavor_line = flavor.map_or_else(String::new, |text| format!("\n💬 {text}"));
        let content = if outcome.spat {
            format!(
                "💢 **{}** spat the {} straight back at you. The temperament of legends. ({} left){}",
                outcome.pet.name,
                food.map_or(item_id, |item| item.display_name),
                outcome.remaining_qty,
                flavor_line
            )
        } else {
            format!(
                "{} **{}** munches the {}. Fullness {} → **{}** `{}` · {} left · {} feeds left today{}",
                food.map_or("🍽️", |item| item.emoji),
                outcome.pet.name,
                food.map_or(item_id, |item| item.display_name),
                outcome.old_hunger,
                outcome.new_hunger,
                hunger_bar(outcome.new_hunger),
                outcome.remaining_qty,
                outcome.feeds_left_today,
                flavor_line
            )
        };
        let _ = interaction.send_followup(ResponsePayload::content(content, Visibility::Ephemeral));
        self.rearm_warning(&outcome.pet, context.now);
    }

    pub fn shop<I: InteractionPort>(&mut self, interaction: &mut I, context: &InteractionContext) {
        if !interaction.defer(Visibility::Ephemeral) {
            return;
        }
        let status = self
            .service
            .status(context.user_id, context.guild_id)
            .unwrap_or(empty_status());
        let species_id = visible_species_id(&status);
        let balance = self.service.balance(context.user_id, context.guild_id);
        let embed = build_shop_embed(status.supplies.as_ref(), species_id, balance);
        let _ = interaction.send_followup(ResponsePayload::embed(embed, Visibility::Ephemeral));
    }

    pub fn rename<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        name: &str,
    ) {
        if !interaction.defer(Visibility::Ephemeral) {
            return;
        }
        match self.service.rename(context.user_id, context.guild_id, name) {
            ServiceResult::Success(pet) => {
                let _ = interaction.send_followup(ResponsePayload::content(
                    format!(
                        "✏️ Henceforth known as **{}** (-{RENAME_COST} {JOPACOIN_EMOTE})",
                        pet.name
                    ),
                    Visibility::Ephemeral,
                ));
            }
            ServiceResult::Failure { error, .. } => {
                let _ = interaction.send_followup(error_payload(error));
            }
        }
    }

    pub fn trinket<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        wear: Option<&str>,
    ) {
        if !interaction.defer(Visibility::Ephemeral) {
            return;
        }
        if let Some(accessory_id) = wear {
            match self
                .service
                .wear_trinket(context.user_id, context.guild_id, accessory_id)
            {
                ServiceResult::Success(equipped) => {
                    let accessory = get_accessory(&equipped);
                    let _ = interaction.send_followup(ResponsePayload::content(
                        format!(
                            "{} Now wearing the **{}**.",
                            accessory.emoji, accessory.display_name
                        ),
                        Visibility::Ephemeral,
                    ));
                }
                ServiceResult::Failure { error, .. } => {
                    let _ = interaction.send_followup(error_payload(error));
                }
            }
            return;
        }
        match self.service.roll_trinket(context.user_id, context.guild_id) {
            ServiceResult::Failure { error, .. } => {
                let _ = interaction.send_followup(error_payload(error));
            }
            ServiceResult::Success(outcome) => {
                let accessory = get_accessory(&outcome.accessory_id);
                let content = if outcome.duplicate {
                    format!(
                        "{} A duplicate **{}** — it dissolves into a partial refund (net -{} {JOPACOIN_EMOTE}). Collection: {}/{}",
                        accessory.emoji,
                        accessory.display_name,
                        outcome.net_cost,
                        outcome.owned_count,
                        ACCESSORIES.len()
                    )
                } else {
                    format!(
                        "🎁 **{}** {} — _{}_ (-{} {JOPACOIN_EMOTE})\nEquipped! Collection: {}/{}",
                        accessory.display_name,
                        accessory.emoji,
                        accessory.blurb,
                        outcome.net_cost,
                        outcome.owned_count,
                        ACCESSORIES.len()
                    )
                };
                let _ = interaction
                    .send_followup(ResponsePayload::content(content, Visibility::Ephemeral));
            }
        }
    }

    #[must_use]
    pub fn trinket_autocomplete(
        &mut self,
        context: &InteractionContext,
        current: &str,
    ) -> Vec<AutocompleteChoice> {
        let current = current.to_lowercase();
        self.service
            .owned_trinkets(context.user_id, context.guild_id)
            .into_iter()
            .filter_map(|accessory_id| {
                let accessory = get_accessory(&accessory_id);
                accessory
                    .display_name
                    .to_lowercase()
                    .contains(&current)
                    .then(|| AutocompleteChoice {
                        name: format!("{} ({})", accessory.display_name, accessory.tier.as_str()),
                        value: accessory_id,
                    })
            })
            .take(25)
            .collect()
    }

    pub fn graveyard<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        target_id: i64,
        target_name: &str,
    ) {
        if !interaction.defer(Visibility::Public) {
            return;
        }
        let pets = self.service.graveyard(target_id, context.guild_id);
        let embed = build_graveyard_embed(&pets, target_name);
        let _ = interaction.send_followup(ResponsePayload::embed(embed, Visibility::Public));
    }

    pub fn leaderboard<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
    ) {
        if !interaction.defer(Visibility::Public) {
            return;
        }
        let pets = self.service.leaderboard(context.guild_id);
        let embed = build_leaderboard_embed(&pets, self.service.decay_per_day(), context.now);
        let _ = interaction.send_followup(ResponsePayload::embed(embed, Visibility::Public));
    }

    pub fn activate_status_button<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        view: &StatusView,
        action: &StatusAction,
    ) {
        if let Err(payload) = view.interaction_check(context.user_id) {
            let _ = interaction.send_immediate(*payload);
            return;
        }
        if !interaction.defer(Visibility::Public) {
            return;
        }
        let flavor_event = match action {
            StatusAction::Feed(item_id) => {
                let result = self.service.feed(view.owner_id, view.guild_id, item_id);
                match result {
                    ServiceResult::Success(outcome) => {
                        if outcome.spat {
                            PetFlavorEvent::Spat
                        } else {
                            PetFlavorEvent::Fed
                        }
                    }
                    ServiceResult::Failure { error, .. } => {
                        let _ = interaction.send_followup(error_payload(error));
                        return;
                    }
                }
            }
            StatusAction::Buy(item_id) => {
                let result = self.service.buy(view.owner_id, view.guild_id, item_id, 1);
                if let ServiceResult::Failure { error, .. } = result {
                    let _ = interaction.send_followup(error_payload(error));
                    return;
                }
                PetFlavorEvent::Status
            }
            StatusAction::SaltLick => {
                let result = self
                    .service
                    .buy(view.owner_id, view.guild_id, SALT_LICK.item_id, 1);
                if let ServiceResult::Failure { error, .. } = result {
                    let _ = interaction.send_followup(error_payload(error));
                    return;
                }
                PetFlavorEvent::Status
            }
        };
        let payload = self.compose_status(
            view.owner_id,
            view.guild_id,
            &context.display_name,
            true,
            flavor_event,
            Visibility::Public,
            context.now,
        );
        let _ = interaction.edit_original(payload);
    }

    pub fn sweep_once<D: DiscordDeliveryPort>(&mut self, discord: &mut D) {
        let Ok(outcome) = self.service.sweep() else {
            return;
        };
        for notice in outcome.hatches {
            let _ = self.deliver_hatch(discord, &notice.pet);
        }
        for notice in outcome.evolutions {
            let _ = self.deliver_evolution(discord, &notice.pet);
        }
        for notice in outcome.deaths {
            if self
                .direct_death_deliveries
                .get(&notice.pet.pet_id)
                .copied()
                .unwrap_or_default()
                > 0
            {
                continue;
            }
            let _ = self.deliver_death(
                discord,
                &notice.pet,
                notice.eating_outcome.as_ref(),
                notice.activated_pet.as_ref(),
            );
        }
        for notice in outcome.refunds {
            let _ = self.deliver_refund(discord, &notice);
        }
    }

    fn check_rate_limit<I: InteractionPort>(
        &mut self,
        interaction: &mut I,
        context: &InteractionContext,
        scope: &str,
    ) -> bool {
        let decision =
            self.rate_limiter
                .check(scope, context.guild_id.unwrap_or_default(), context.user_id);
        if decision.allowed {
            return true;
        }
        let _ = interaction.send_immediate(ResponsePayload::content(
            format!("⏳ Please wait {}s.", decision.retry_after_seconds),
            Visibility::Ephemeral,
        ));
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn compose_status(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        owner_name: &str,
        with_view: bool,
        flavor_event: PetFlavorEvent,
        visibility: Visibility,
        now: i64,
    ) -> ResponsePayload {
        let status = self
            .service
            .status(discord_id, guild_id)
            .unwrap_or(empty_status());
        let next_fee = self.service.next_adoption_fee(discord_id, guild_id);
        let flavor_pet = status.pet.as_ref().or(status.last_dead.as_ref());
        let flavor = flavor_pet.and_then(|pet| {
            let actual_event = if status.pet.is_some() {
                flavor_event
            } else {
                PetFlavorEvent::Died
            };
            self.flavor.generate(actual_event, pet, Some(&status))
        });
        let mut embed = build_status_embed(
            &status,
            self.service.decay_per_day(),
            now,
            owner_name,
            next_fee,
        );
        if let Some(flavor) = flavor {
            embed.field("💬 Cama chatter", flavor, false);
        }
        let attachment = status.pet.as_ref().map_or_else(
            || status.last_dead.as_ref().map(tombstone_attachment),
            |pet| {
                Some(if status.stage == Some(PetStage::Egg) {
                    egg_attachment(pet.pet_id)
                } else {
                    pet_attachment(pet)
                })
            },
        );
        let view = if with_view {
            status.pet.as_ref().map(|pet| {
                let game_date =
                    cama_domain::game_date::game_date_for_timestamp(now as f64).unwrap_or_default();
                let can_feed = status.stage != Some(PetStage::Egg)
                    && pet.feeds_used_on(&game_date) < FEED_CAP_PER_DAY;
                StatusView::new(
                    discord_id,
                    guild_id,
                    status.supplies.as_ref(),
                    visible_species_id(&status),
                    can_feed,
                )
            })
        } else {
            None
        };
        if let Some(pet) = status.pet.as_ref() {
            self.rearm_warning(pet, now);
        }
        ResponsePayload {
            embed: Some(embed),
            attachment,
            view,
            visibility,
            content: None,
        }
    }

    fn rearm_warning(&mut self, pet: &Pet, _now: i64) {
        if pet.species == UNHATCHED_SPECIES {
            return;
        }
        let Some(reminders) = self.reminders.as_mut() else {
            return;
        };
        let Ok(crossing) = self.service.warning_crossing_for(pet) else {
            return;
        };
        let preference_enabled = reminders.pet_enabled(pet.discord_id, pet.guild_id);
        reminders.schedule_pet_reminder(
            pet.discord_id,
            pet.guild_id,
            crossing,
            &pet.name,
            preference_enabled,
        );
    }

    fn channel_for<D: DiscordDeliveryPort>(&self, discord: &mut D, guild_id: i64) -> Option<i64> {
        self.config
            .pet_channel_id
            .and_then(|channel_id| discord.resolve_text_channel(guild_id, channel_id))
    }

    fn deliver_hatch<D: DiscordDeliveryPort>(
        &mut self,
        discord: &mut D,
        pet: &Pet,
    ) -> Result<(), DeliveryError> {
        if let Some(channel_id) = self.channel_for(discord, pet.guild_id) {
            let flavor = self.flavor.generate(PetFlavorEvent::Hatched, pet, None);
            let mut embed = build_hatch_embed(pet);
            if let Some(flavor) = flavor {
                embed.field("💬 Cama chatter", flavor, false);
            }
            let payload = ResponsePayload {
                embed: Some(embed),
                attachment: Some(pet_attachment(pet)),
                ..ResponsePayload::default()
            };
            match discord.send_channel(channel_id, payload) {
                Ok(()) | Err(DeliveryError::Forbidden) => {}
                Err(error) => return Err(error),
            }
        }
        let _ = self.service.mark_hatch_announced(pet);
        self.rearm_warning(pet, pet.hatched_at);
        Ok(())
    }

    fn deliver_evolution<D: DiscordDeliveryPort>(
        &mut self,
        discord: &mut D,
        pet: &Pet,
    ) -> Result<(), DeliveryError> {
        if let Some(channel_id) = self.channel_for(discord, pet.guild_id) {
            let flavor = self.flavor.generate(PetFlavorEvent::Evolved, pet, None);
            let mut embed = build_evolution_embed(pet);
            if let Some(flavor) = flavor {
                embed.field("💬 Cama chatter", flavor, false);
            }
            let payload = ResponsePayload {
                embed: Some(embed),
                attachment: Some(pet_attachment(pet)),
                ..ResponsePayload::default()
            };
            match discord.send_channel(channel_id, payload) {
                Ok(()) | Err(DeliveryError::Forbidden) => {}
                Err(error) => return Err(error),
            }
        }
        let _ = self.service.mark_evolution_announced(pet);
        Ok(())
    }

    fn deliver_death<D: DiscordDeliveryPort>(
        &mut self,
        discord: &mut D,
        pet: &Pet,
        eating_outcome: Option<&BTreeMap<String, i64>>,
        activated_pet: Option<&Pet>,
    ) -> Result<(), DeliveryError> {
        let channel_id = self.channel_for(discord, pet.guild_id);
        let dm_enabled = self
            .reminders
            .as_mut()
            .is_some_and(|reminders| reminders.pet_enabled(pet.discord_id, pet.guild_id));
        let needs_flavor = eating_outcome.is_none() && (channel_id.is_some() || dm_enabled);
        let flavor = needs_flavor
            .then(|| self.flavor.generate(PetFlavorEvent::Died, pet, None))
            .flatten();
        let mut embed = eating_outcome.map_or_else(
            || build_death_embed(pet),
            |outcome| build_eating_outcome_embed(pet, outcome),
        );
        if eating_outcome.is_none()
            && let Some(flavor) = flavor
        {
            embed.field("💬 Cama chatter", flavor, false);
        }
        if let Some(activated_pet) = activated_pet {
            embed.field(
                "🏡 New active cama",
                format!(
                    "**{}** was automatically activated from the stable.",
                    activated_pet.name
                ),
                false,
            );
        }
        if let Some(channel_id) = channel_id {
            let payload = ResponsePayload::embed(embed.clone(), Visibility::Public);
            match discord.send_channel(channel_id, payload) {
                Ok(()) | Err(DeliveryError::Forbidden) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(reminders) = self.reminders.as_mut() {
            reminders.cancel_pet_reminder(pet.discord_id, pet.guild_id);
        }
        if dm_enabled {
            let _ = discord.send_dm(
                pet.discord_id,
                ResponsePayload::embed(embed, Visibility::Public),
            );
        }
        let _ = self.service.mark_death_announced(pet);
        Ok(())
    }

    fn deliver_refund<D: DiscordDeliveryPort>(
        &mut self,
        discord: &mut D,
        notice: &RefundNotice,
    ) -> Result<(), DeliveryError> {
        if let Some(channel_id) = self.channel_for(discord, notice.guild_id) {
            let payload = ResponsePayload::embed(build_refund_embed(notice), Visibility::Public);
            match discord.send_channel(channel_id, payload) {
                Ok(()) | Err(DeliveryError::Forbidden) => {}
                Err(error) => return Err(error),
            }
        }
        let _ = self.service.mark_refund_announced(notice);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutocompleteChoice {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationPreferences {
    pet_enabled: BTreeSet<(i64, i64)>,
}

impl NotificationPreferences {
    #[must_use]
    pub fn pet_enabled(&self, discord_id: i64, guild_id: i64) -> bool {
        self.pet_enabled.contains(&(discord_id, guild_id))
    }

    pub fn set_pet_enabled(&mut self, discord_id: i64, guild_id: i64, enabled: bool) {
        if enabled {
            self.pet_enabled.insert((discord_id, guild_id));
        } else {
            self.pet_enabled.remove(&(discord_id, guild_id));
        }
    }

    #[must_use]
    pub fn enabled_pet_users(&self, guild_id: i64) -> Vec<i64> {
        self.pet_enabled
            .iter()
            .filter_map(|(discord_id, candidate_guild)| {
                (*candidate_guild == guild_id).then_some(*discord_id)
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderTask {
    pub crossing: i64,
    pub pet_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderCoordinator {
    now: i64,
    tasks: BTreeMap<(i64, i64, &'static str), ReminderTask>,
}

impl ReminderCoordinator {
    #[must_use]
    pub fn new(now: i64) -> Self {
        Self {
            now,
            tasks: BTreeMap::new(),
        }
    }

    pub fn set_now(&mut self, now: i64) {
        self.now = now;
    }

    pub fn schedule_pet_reminder(
        &mut self,
        discord_id: i64,
        guild_id: i64,
        crossing: i64,
        pet_name: impl Into<String>,
        preference_enabled: bool,
    ) {
        let key = (discord_id, guild_id, "pet");
        self.tasks.remove(&key);
        if preference_enabled && crossing > self.now {
            self.tasks.insert(
                key,
                ReminderTask {
                    crossing,
                    pet_name: pet_name.into(),
                },
            );
        }
    }

    #[must_use]
    pub fn has_pet_task(&self, discord_id: i64, guild_id: i64) -> bool {
        self.tasks.contains_key(&(discord_id, guild_id, "pet"))
    }

    pub fn reschedule_pet_warnings<P: PetWarningSource>(
        &mut self,
        preferences: &NotificationPreferences,
        guild_ids: &[i64],
        mut pet_service: Option<&mut P>,
    ) {
        let Some(service) = pet_service.as_mut() else {
            return;
        };
        for guild_id in guild_ids {
            for discord_id in preferences.enabled_pet_users(*guild_id) {
                if let Some((crossing, name)) = service.warning_crossing(discord_id, *guild_id) {
                    self.schedule_pet_reminder(discord_id, *guild_id, crossing, name, true);
                }
            }
        }
    }
}

pub trait PetWarningSource {
    fn warning_crossing(&mut self, discord_id: i64, guild_id: i64) -> Option<(i64, String)>;
}

#[must_use]
pub fn configured_pet_channel<D: DiscordDeliveryPort>(
    configured_channel_id: Option<i64>,
    guild_id: i64,
    discord: &mut D,
) -> Option<i64> {
    configured_channel_id.and_then(|channel_id| discord.resolve_text_channel(guild_id, channel_id))
}

#[must_use]
pub fn build_adoption_embed(outcome: &AdoptOutcome, owner_name: &str) -> Embed {
    let gilded = outcome.egg_tier == "gilded";
    let title = if gilded {
        "🥚 A gilded egg!"
    } else {
        "🥚 A mysterious egg!"
    };
    let description = if outcome.upgraded {
        format!(
            "**{owner_name}** upgraded their egg to a **gilded egg** and named it **{}** for {GILDED_EGG_PREMIUM} {JOPACOIN_EMOTE}.\nIt still hatches <t:{}:R>. What's inside? Nobody knows. Not even the egg.",
            outcome.pet.name, outcome.pet.hatched_at
        )
    } else {
        let flair = if gilded { "a **gilded egg**" } else { "an egg" };
        let pity = if outcome.pity_active {
            "\n✨ The nonprofit took pity: this egg can't be Common."
        } else {
            ""
        };
        let hatch = if outcome.pet.is_active {
            format!("It hatches <t:{}:R>.", outcome.pet.hatched_at)
        } else {
            "It is waiting safely in the stable; its hatch clock resumes when activated.".to_owned()
        };
        format!(
            "**{owner_name}** adopted {flair} and named it **{}** for {} {JOPACOIN_EMOTE}.\n{hatch} What's inside? Nobody knows. Not even the egg.{pity}",
            outcome.pet.name, outcome.pet.adopt_fee
        )
    };
    let mut embed = Embed::new(title, description, EmbedColor::Gold);
    embed.image = Some(format!("attachment://pet-egg-{}.png", outcome.pet.pet_id));
    embed
}

#[must_use]
pub fn build_altar_preview_embed(preview: &SacrificePreview) -> Embed {
    let weights = preview.weights;
    let odds = [
        ("Common", weights.common),
        ("Uncommon", weights.uncommon),
        ("Rare", weights.rare),
        ("Legendary", weights.legendary),
    ]
    .into_iter()
    .map(|(tier, weight)| format!("{tier} {weight}%"))
    .collect::<Vec<_>>()
    .join(" · ");
    Embed::new(
        format!("🩸 The altar hungers for {}", preview.pet.name),
        format!(
            "Sacrifice your {} for an immediate reroll with upgraded odds:\n**{odds}**\n\nFee: **{}** {JOPACOIN_EMOTE} — and this death still escalates your future adoption fees. The altar gives, but it remembers.",
            pet_species_label(&preview.pet),
            preview.fee
        ),
        EmbedColor::Slate,
    )
}

#[must_use]
pub fn build_altar_cancel_embed(preview: &SacrificePreview) -> Embed {
    Embed::new(
        "🕯️ The altar goes cold",
        format!("**{}** lives to graze another day.", preview.pet.name),
        EmbedColor::Slate,
    )
}

#[must_use]
pub fn build_altar_success_embed(outcome: &SacrificeOutcome) -> Embed {
    let mut embed = Embed::new(
        format!("🩸 {} was given to the altar", outcome.dead_pet.name),
        format!(
            "The altar accepts **{}** ({}).\nIn the ashes, a new egg: **{}**, hatching <t:{}:R> — its species already chosen, already hidden.\nFee: {} {JOPACOIN_EMOTE}.",
            outcome.dead_pet.name,
            pet_species_label(&outcome.dead_pet),
            outcome.new_pet.name,
            outcome.new_pet.hatched_at,
            outcome.fee,
        ),
        EmbedColor::Gold,
    );
    embed.image = Some(format!(
        "attachment://pet-altar-{}.png",
        outcome.dead_pet.pet_id
    ));
    embed
}

#[must_use]
pub fn build_status_embed(
    status: &PetStatus,
    decay_per_day: i64,
    now: i64,
    owner_name: &str,
    next_fee: i64,
) -> Embed {
    build_status_embed_with_details(StatusEmbedRequest {
        status,
        decay_per_day,
        now,
        owner_name,
        next_fee,
        brawl_record: None,
        career: None,
        solo_training_sessions: None,
    })
}

/// All facts needed to render the enriched live status panel.  Keeping the
/// request typed makes it difficult for the runtime adapter to mix fields from
/// different snapshots and keeps the projection API below its argument limit.
pub struct StatusEmbedRequest<'a> {
    pub status: &'a PetStatus,
    pub decay_per_day: i64,
    pub now: i64,
    pub owner_name: &'a str,
    pub next_fee: i64,
    pub brawl_record: Option<(i64, i64)>,
    pub career: Option<&'a crate::pet_brawl::CareerSummary>,
    pub solo_training_sessions: Option<i64>,
}

/// Build the status panel with the optional brawl/career projections that the
/// live Python cog loads alongside the pet status row.  The simple wrapper
/// above remains for callers that do not compose the brawl service.
#[must_use]
pub fn build_status_embed_with_details(request: StatusEmbedRequest<'_>) -> Embed {
    let StatusEmbedRequest {
        status,
        decay_per_day,
        now,
        owner_name,
        next_fee,
        brawl_record,
        career,
        solo_training_sessions,
    } = request;
    let Some(pet) = status.pet.as_ref() else {
        return if let Some(dead) = status.last_dead.as_ref() {
            let species = get_species(&dead.species);
            let calling = pet_calling_label(dead);
            let mut identity = pet_species_label(dead);
            if let Some(calling) = calling {
                identity.push_str(&format!(" · {calling}"));
            }
            let died_at = dead.died_at.unwrap_or(dead.adopted_at);
            let death_phrase = if dead.death_cause.as_deref() == Some("eaten") {
                "was eaten".to_owned()
            } else {
                format!(
                    "died of {}",
                    dead.death_cause.as_deref().unwrap_or("starvation")
                )
            };
            let mut embed = Embed::new(
                format!("In memoriam: {}", dead.name),
                format!(
                    "{identity} · {death_phrase}, {} — <t:{died_at}:R>\n\n_{}_\n\nF.\n\nAdopt again with `/pet adopt` — {next_fee} {JOPACOIN_EMOTE}",
                    format_age(dead.age_seconds(died_at)),
                    species.blurb,
                ),
                EmbedColor::Slate,
            );
            embed.image = Some(format!("attachment://pet-tombstone-{}.png", dead.pet_id));
            embed
        } else {
            Embed::new(
                format!("{owner_name} has no cama"),
                format!(
                    "A hollow feeling. A camaless existence.\n\nAdopt a mysterious egg with `/pet adopt` — {next_fee} {JOPACOIN_EMOTE}"
                ),
                EmbedColor::Blue,
            )
        };
    };
    if status.stage == Some(PetStage::Egg) {
        let mut embed = Embed::new(
            format!("🥚 {}", pet.name),
            format!(
                "A mysterious egg, warm to the touch.\nHatches <t:{}:R>. What's inside? Nobody knows.",
                pet.hatched_at
            ),
            EmbedColor::Gold,
        );
        embed.field("Supplies", supplies_line(status.supplies.as_ref()), false);
        embed.image = Some(format!("attachment://pet-egg-{}.png", pet.pet_id));
        return embed;
    }
    let mood = status.mood.unwrap_or(PetMood::Content);
    let species = get_species(&pet.species);
    let profile = pet
        .evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .map(calling_profile);
    let color = profile.map_or_else(
        || {
            if species.tier == SpeciesTier::Legendary {
                EmbedColor::Custom(0xE9_1E_63)
            } else {
                match mood {
                    PetMood::Happy => EmbedColor::Green,
                    PetMood::Content => EmbedColor::Blue,
                    PetMood::Hungry => EmbedColor::Orange,
                    PetMood::Starving => EmbedColor::Red,
                }
            }
        },
        |profile| EmbedColor::Custom(profile.color),
    );
    let calling = profile.map_or_else(String::new, |profile| {
        format!(" · {} {}", profile.emoji, profile.label)
    });
    let mut embed = Embed::new(
        format!("{} — {}{}", pet.name, pet_species_label(pet), calling),
        format!("_{}_", species.blurb),
        color,
    );
    // Keep the same compact title-case stage text as discord.py.
    let stage = status.stage.unwrap_or(PetStage::Baby);
    embed.field(
        "Stage",
        format!(
            "{} · {}",
            stage_title(stage),
            format_age(status.age_seconds)
        ),
        true,
    );
    let mut mood_text = format!("{} {}", mood_emoji(mood), stage_title_mood(mood));
    if pet.pampered_until.is_some_and(|until| until > now) {
        mood_text.push_str(" (🧂 pampered)");
    }
    embed.field("Mood", mood_text, true);
    embed.field(
        "Fullness",
        format!("`{}` {}/100", hunger_bar(status.hunger), status.hunger),
        false,
    );
    embed.field(
        "Mining",
        format!(
            "{}/{} blocks banked · +{}/day",
            format_work_blocks(status.dig_work_units),
            DIG_WORK_CAP_BLOCKS,
            status.dig_work_rate
        ),
        false,
    );
    if let Some(upbringing) = hint_text(status.evolution_hint.as_deref()) {
        embed.field("Upbringing", upbringing, false);
    }
    if let Some(profile) = profile {
        embed.field(
            "Calling",
            format!("_{}_\nPaths: {}", profile.blurb, pet_calling_paths(pet)),
            false,
        );
    }
    if status.hunger <= WARNING_HUNGER {
        let starvation_at = pet
            .starvation_time(decay_per_day)
            .unwrap_or(now.saturating_add(DAY_SECONDS));
        embed.field(
            "⚠️ Warning",
            format!("Will starve <t:{starvation_at}:R> without food!"),
            false,
        );
    }
    embed.field("Supplies", supplies_line(status.supplies.as_ref()), false);
    if let Some(accessory) = pet.accessory.as_deref() {
        let accessory = get_accessory(accessory);
        embed.field(
            "Trinket",
            format!("{} {}", accessory.emoji, accessory.display_name),
            true,
        );
    }
    if let Some((wins, losses)) = brawl_record
        && (wins != 0 || losses != 0)
    {
        embed.field("⚔️ Brawls", format!("{wins}W · {losses}L"), true);
    }
    if let Some(career) = career {
        let titles = [career.rank.as_deref(), career.build.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let title_line = if titles.is_empty() {
            String::new()
        } else {
            format!("\n{titles}")
        };
        let level = career.strength + career.intelligence + career.dexterity;
        let solo_line = solo_training_sessions.map_or_else(String::new, |sessions| {
            format!("\nSolo: {sessions}/{SOLO_TRAINING_SESSION_CAP} banked · `/pet train`")
        });
        embed.field(
            "🏋️ Training",
            format!(
                "Level {level}/4 · {}/20 XP\nSTR {} · INT {} · DEX {}{solo_line}{title_line}",
                career.xp, career.strength, career.intelligence, career.dexterity,
            ),
            false,
        );
    }
    embed.footer = Some(format!(
        "{}/{} feeds left today",
        (FEED_CAP_PER_DAY
            - pet.feeds_used_on(
                &cama_domain::game_date::game_date_for_timestamp(now as f64).unwrap_or_default(),
            ))
        .max(0),
        FEED_CAP_PER_DAY
    ));
    embed.image = Some(format!("attachment://pet-{}.png", pet.pet_id));
    embed
}

fn mood_emoji(mood: PetMood) -> &'static str {
    match mood {
        PetMood::Happy => "😊",
        PetMood::Content => "🙂",
        PetMood::Hungry => "😟",
        PetMood::Starving => "😫",
    }
}

fn stage_title(stage: PetStage) -> &'static str {
    match stage {
        PetStage::Egg => "Egg",
        PetStage::Baby => "Baby",
        PetStage::Adult => "Adult",
    }
}

fn stage_title_mood(mood: PetMood) -> &'static str {
    match mood {
        PetMood::Happy => "Happy",
        PetMood::Content => "Content",
        PetMood::Hungry => "Hungry",
        PetMood::Starving => "Starving",
    }
}

#[must_use]
pub fn build_shop_embed(
    supplies: Option<&BTreeMap<String, i64>>,
    species_id: &str,
    balance: Option<i64>,
) -> Embed {
    let mut embed = Embed::new(
        "🛒 Cama Pantry",
        "Buy with `/pet buy` or the buttons on `/pet status`.",
        EmbedColor::Blue,
    );
    for food in FOOD_ITEMS {
        let cost = food_cost(species_id, food.item_id).unwrap_or(food.cost);
        let discount = if cost < food.cost {
            " (discounted!)"
        } else {
            ""
        };
        let owned = supplies
            .and_then(|stock| stock.get(food.item_id))
            .copied()
            .unwrap_or_default();
        embed.field(
            format!(
                "{} {} — {cost} {JOPACOIN_EMOTE}{discount}",
                food.emoji, food.display_name
            ),
            format!(
                "+{} fullness · owned ×{owned}\n_{}_",
                food.restore, food.blurb
            ),
            false,
        );
    }
    let lick_cost = food_cost(species_id, SALT_LICK.item_id).unwrap_or(SALT_LICK.cost);
    embed.field(
        format!(
            "{} {} — {lick_cost} {JOPACOIN_EMOTE}",
            SALT_LICK.emoji, SALT_LICK.display_name
        ),
        format!(
            "Applies immediately: pampered mood for 12h.\n_{}_",
            SALT_LICK.blurb
        ),
        false,
    );
    if let Some(balance) = balance {
        embed.footer = Some(format!("Balance: {balance} jopacoin"));
    }
    embed
}

#[must_use]
pub fn build_hatch_embed(pet: &Pet) -> Embed {
    let species = get_species(&pet.species);
    let reveal = match species.tier {
        SpeciesTier::Common => "It's a",
        SpeciesTier::Uncommon => "Oh! It's a",
        SpeciesTier::Rare => "✨ Incredible — it's a",
        SpeciesTier::Legendary => "🎆 UNBELIEVABLE — it's the",
    };
    let mut embed = Embed::new(
        format!("🐣 {} hatched!", pet.name),
        format!(
            "<@{}>'s egg cracked open... **{reveal} {}!**\n\n_{}_",
            pet.discord_id, species.display_name, species.blurb
        ),
        if species.tier == SpeciesTier::Legendary {
            EmbedColor::Custom(0xE9_1E_63)
        } else {
            EmbedColor::Green
        },
    );
    embed.image = Some(format!("attachment://pet-{}.png", pet.pet_id));
    embed
}

#[must_use]
pub fn build_evolution_embed(pet: &Pet) -> Embed {
    let calling = pet
        .evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .unwrap_or(PetCalling::Wayfarer);
    let profile = calling_profile(calling);
    let paths = pet_calling_paths(pet);
    let mut embed = Embed::new(
        format!(
            "{} {} evolved — {}!",
            profile.emoji, pet.name, profile.label
        ),
        format!(
            "<@{}>'s {} found a calling.\n\n**{paths}**\n_{}_",
            pet.discord_id,
            pet_species_label(pet),
            profile.blurb
        ),
        EmbedColor::Custom(profile.color),
    );
    embed.image = Some(format!("attachment://pet-{}.png", pet.pet_id));
    embed
}

#[must_use]
pub fn build_death_embed(pet: &Pet) -> Embed {
    let cause = match pet.death_cause.as_deref() {
        Some("sacrifice") => "was given to the altar",
        Some("eaten") => "was eaten",
        _ => "starved",
    };
    let calling = pet_calling_label(pet);
    let calling = calling.map_or_else(String::new, |label| format!(" · {label}"));
    let died_at = pet.died_at.unwrap_or(pet.adopted_at);
    let mut embed = Embed::new(
        format!("🪦 {} has died", pet.name),
        format!(
            "<@{}>'s {}{calling} {cause}, {}.\nTime of death: <t:{died_at}:f>. F.",
            pet.discord_id,
            pet_species_label(pet),
            format_age(pet.age_seconds(died_at)),
        ),
        EmbedColor::Slate,
    );
    embed.image = Some(format!("attachment://pet-tombstone-{}.png", pet.pet_id));
    embed
}

#[must_use]
pub fn build_eating_outcome_embed(pet: &Pet, outcome: &BTreeMap<String, i64>) -> Embed {
    let reward = outcome.get("reward").copied().unwrap_or_default();
    let added = outcome
        .get("penalty_games_added")
        .copied()
        .unwrap_or_default();
    let remaining = outcome
        .get("penalty_games_remaining")
        .copied()
        .unwrap_or_default();
    let balance = outcome.get("new_balance").copied().unwrap_or_default();
    Embed::new(
        format!("🍖 {} was delicious", pet.name),
        format!(
            "You received **{}** {JOPACOIN_EMOTE}.\n\nBad karma adds **{added}** game(s) to your sentence (**{remaining} remaining**). Your future earnings are taxed until you win your way free.\n\nBalance: **{}** {JOPACOIN_EMOTE}.",
            format_integer(reward),
            format_integer(balance)
        ),
        EmbedColor::Slate,
    )
}

fn parse_calling(value: &str) -> Option<PetCalling> {
    PetCalling::ALL
        .into_iter()
        .find(|calling| calling.as_str() == value)
}

fn parse_instinct(value: &str) -> Option<PetInstinct> {
    PetInstinct::ALL
        .into_iter()
        .find(|instinct| instinct.as_str() == value)
}

fn pet_species_label(pet: &Pet) -> String {
    let species = get_species(&pet.species);
    let badge = match species.tier {
        SpeciesTier::Common => "",
        SpeciesTier::Uncommon => "🔹 ",
        SpeciesTier::Rare => "🔮 ",
        SpeciesTier::Legendary => "👑 ",
    };
    format!("{badge}{}", species.display_name)
}

fn pet_calling_label(pet: &Pet) -> Option<String> {
    pet.evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .map(|calling| {
            let profile = calling_profile(calling);
            format!("{} {}", profile.emoji, profile.label)
        })
}

fn pet_calling_paths(pet: &Pet) -> String {
    let paths = [
        pet.evolution_primary.as_deref(),
        pet.evolution_secondary.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(parse_instinct)
    .map(instinct_label)
    .collect::<Vec<_>>();
    if paths.is_empty() {
        "A little of every path".to_owned()
    } else {
        paths.join(" + ")
    }
}

#[must_use]
pub fn build_refund_embed(notice: &RefundNotice) -> Embed {
    let mut payouts = notice.payouts.clone();
    payouts.sort_by_key(|payout| std::cmp::Reverse(payout.amount));
    let lines = payouts
        .iter()
        .take(15)
        .map(|payout| {
            format!(
                "<@{}> spent {} on care → rolled **{}%** → +{} {JOPACOIN_EMOTE}",
                payout.discord_id, payout.consumed_jc, payout.multiplier_pct, payout.amount
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let scaled = if notice.scaled_down {
        "\n\n_The nonprofit couldn't cover the full total — refunds were scaled down pro-rata._"
    } else {
        ""
    };
    Embed::new(
        format!("🏦 Weekly pet care refund — {}", notice.week_key),
        format!("The jopacoin nonprofit rewards responsible cama ownership.\n\n{lines}{scaled}"),
        EmbedColor::Green,
    )
}

#[must_use]
pub fn build_graveyard_embed(pets: &[Pet], owner_name: &str) -> Embed {
    build_graveyard_embed_with_dex(pets, owner_name, None, None)
}

#[must_use]
pub fn build_graveyard_embed_with_dex(
    pets: &[Pet],
    owner_name: &str,
    camadex: Option<(&[String], usize)>,
    callingdex: Option<(&[PetCalling], usize)>,
) -> Embed {
    let mut embed = if pets.is_empty() {
        Embed::new(
            format!("🪦 {owner_name}'s graveyard"),
            "No pets have died. A spotless record. So far.",
            EmbedColor::Blue,
        )
    } else {
        let lines = pets
            .iter()
            .map(|pet| {
                let calling = pet_calling_label(pet)
                    .map_or_else(String::new, |calling| format!(" · {calling}"));
                let died_at = pet.died_at.unwrap_or(pet.adopted_at);
                format!(
                    "**{}** · {}{calling} · {} · <t:{died_at}:R>",
                    pet.name,
                    pet_species_label(pet),
                    format_age(pet.age_seconds(died_at)),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Embed::new(
            format!("🪦 {owner_name}'s graveyard"),
            format!("{lines}\n\nF."),
            EmbedColor::Slate,
        )
    };
    if let Some((raised, total)) = camadex {
        let discovered = SPECIES
            .iter()
            .filter(|species| raised.iter().any(|raised| raised == species.species_id))
            .count();
        let entries = SPECIES
            .iter()
            .map(|species| {
                if raised.iter().any(|raised| raised == species.species_id) {
                    species.display_name.to_owned()
                } else {
                    "???".to_owned()
                }
            })
            .collect::<Vec<_>>();
        embed.field(
            format!("📖 Camadex {discovered}/{total}"),
            entries.join(" · "),
            false,
        );
    }
    if let Some((raised, total)) = callingdex {
        let discovered = PetCalling::ALL
            .into_iter()
            .filter(|calling| raised.contains(calling))
            .count();
        let entries = PetCalling::ALL
            .into_iter()
            .map(|calling| {
                if raised.contains(&calling) {
                    calling_profile(calling).label.to_owned()
                } else {
                    "???".to_owned()
                }
            })
            .collect::<Vec<_>>();
        embed.field(
            format!("Callings {discovered}/{total}"),
            entries.join(" · "),
            false,
        );
    }
    embed
}

#[must_use]
pub fn build_leaderboard_embed(pets: &[Pet], decay_per_day: i64, now: i64) -> Embed {
    build_leaderboard_embed_with_records(pets, decay_per_day, now, None)
}

#[must_use]
pub fn build_leaderboard_embed_with_records(
    pets: &[Pet],
    decay_per_day: i64,
    now: i64,
    records: Option<&BTreeMap<i64, (i64, i64)>>,
) -> Embed {
    if pets.is_empty() {
        return Embed::new(
            "🏆 Oldest living camas",
            "No pets have hatched yet. `/pet adopt` to be first.",
            EmbedColor::Blue,
        );
    }
    let lines = pets
        .iter()
        .enumerate()
        .map(|(index, pet)| {
            let mood = pet.mood(now, decay_per_day);
            let rank = match index {
                0 => "🥇".to_owned(),
                1 => "🥈".to_owned(),
                2 => "🥉".to_owned(),
                _ => format!("{}.", index + 1),
            };
            let record = records
                .and_then(|records| records.get(&pet.pet_id))
                .copied()
                .unwrap_or_default();
            let record_text = if record.0 != 0 || record.1 != 0 {
                format!(" · ⚔️ {}W-{}L", record.0, record.1)
            } else {
                String::new()
            };
            format!(
                "{rank} **{}** ({}) — <@{}> · {} {}{record_text}",
                pet.name,
                pet_species_label(pet),
                pet.discord_id,
                format_age(pet.age_seconds(now)),
                mood_emoji(mood),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Embed::new("🏆 Oldest living camas", lines, EmbedColor::Green)
}

#[must_use]
pub fn hunger_bar(hunger: i64) -> String {
    let clamped = hunger.clamp(0, 100);
    let quotient = clamped / 10;
    let remainder = clamped % 10;
    let rounded = quotient + i64::from(remainder > 5 || (remainder == 5 && quotient % 2 != 0));
    let filled = usize::try_from(rounded.clamp(0, 10)).unwrap_or_default();
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

fn food_by_id(item_id: &str) -> Option<&'static cama_domain::pet::PetFood> {
    FOOD_ITEMS.iter().find(|food| food.item_id == item_id)
}

fn empty_status() -> PetStatus {
    PetStatus {
        pet: None,
        hunger: 0,
        stage: None,
        mood: None,
        age_seconds: 0,
        supplies: Some(BTreeMap::new()),
        last_dead: None,
        dig_work_units: 0,
        dig_work_rate: 0,
        evolution_hint: None,
    }
}

fn error_payload(error: impl AsRef<str>) -> ResponsePayload {
    ResponsePayload::content(format!("❌ {}", error.as_ref()), Visibility::Ephemeral)
}

fn visible_species_id(status: &PetStatus) -> &str {
    status.pet.as_ref().map_or("", |pet| {
        if status.stage == Some(PetStage::Egg) {
            ""
        } else {
            pet.species.as_str()
        }
    })
}

fn supplies_line(supplies: Option<&BTreeMap<String, i64>>) -> String {
    let Some(supplies) = supplies else {
        return "None — visit `/pet shop`".to_owned();
    };
    let parts = FOOD_ITEMS
        .iter()
        .filter_map(|food| {
            let quantity = supplies.get(food.item_id).copied().unwrap_or_default();
            (quantity > 0).then(|| format!("{} {} ×{quantity}", food.emoji, food.display_name))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "None — visit `/pet shop`".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn egg_attachment(pet_id: i64) -> Attachment {
    Attachment {
        filename: format!("pet-egg-{pet_id}.png"),
    }
}

fn pet_attachment(pet: &Pet) -> Attachment {
    Attachment {
        filename: format!("pet-{}.png", pet.pet_id),
    }
}

fn tombstone_attachment(pet: &Pet) -> Attachment {
    Attachment {
        filename: format!("pet-tombstone-{}.png", pet.pet_id),
    }
}

fn format_age(age_seconds: i64) -> String {
    let days = age_seconds / DAY_SECONDS;
    if days >= 1 {
        return format!("{days} day{} old", if days == 1 { "" } else { "s" });
    }
    let hours = age_seconds / 3_600;
    format!("{hours} hour{} old", if hours == 1 { "" } else { "s" })
}

fn format_work_blocks(units: i64) -> String {
    let tenths = units.saturating_mul(10) / DIG_WORK_UNITS_PER_BLOCK;
    if tenths % 10 == 0 {
        (tenths / 10).to_string()
    } else {
        format!("{}.{}", tenths / 10, tenths.abs() % 10)
    }
}

fn format_integer(value: i64) -> String {
    let negative = value.is_negative();
    let digits = value.unsigned_abs().to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3 + usize::from(negative));
    if negative {
        result.push('-');
    }
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use cama_domain::pet::{
        DeathNotice, EGG_HATCH_SECONDS, EvolutionNotice, HatchNotice, MAX_LIVING_PETS,
        PET_SWITCH_COOLDOWN_SECONDS, PET_SWITCH_COST, PetBase, RefundPayout,
    };

    use super::*;

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 999;
    const OWNER: i64 = 100;

    fn make_pet() -> Pet {
        Pet::new(PetBase {
            pet_id: 1,
            discord_id: 111,
            guild_id: GUILD,
            name: "Blep".to_owned(),
            species: "common_cama".to_owned(),
            adopted_at: T0,
            hatched_at: T0 + EGG_HATCH_SECONDS,
            adopt_fee: 20,
            last_fed_at: T0 + EGG_HATCH_SECONDS,
            hunger_at_last_fed: 100,
            times_fed: 0,
            feeds_today: 0,
            feed_date: None,
            week_consumed_jc: 0,
            week_key: None,
            prev_week_consumed_jc: 0,
            prev_week_key: None,
            pampered_until: None,
            accessory: None,
            dig_work_units: 0,
            dig_work_at: T0 + EGG_HATCH_SECONDS,
            aegis_used: 0,
            hatch_announced_at: None,
            died_at: None,
            death_cause: None,
            death_announced_at: None,
        })
    }

    fn owner_pet() -> Pet {
        let mut pet = make_pet();
        pet.discord_id = OWNER;
        pet
    }

    #[derive(Clone, Debug)]
    struct FakeService {
        now: i64,
        decay_per_day: i64,
        sweep_result: Result<SweepOutcome, String>,
        registered: BTreeSet<(i64, i64)>,
        balances: BTreeMap<(i64, i64), i64>,
        active: BTreeMap<(i64, i64), Pet>,
        stabled: BTreeMap<(i64, i64), Vec<Pet>>,
        supplies: BTreeMap<(i64, i64), BTreeMap<String, i64>>,
        accessories: BTreeMap<(i64, i64), BTreeSet<String>>,
        graveyard: Vec<Pet>,
        next_pet_id: i64,
        next_accessory: String,
        marked_hatches: Vec<i64>,
        marked_evolutions: Vec<i64>,
        marked_deaths: Vec<i64>,
        marked_refunds: Vec<(i64, String)>,
    }

    impl Default for FakeService {
        fn default() -> Self {
            let mut registered = BTreeSet::new();
            registered.insert((OWNER, GUILD));
            let mut balances = BTreeMap::new();
            balances.insert((OWNER, GUILD), 1_000);
            Self {
                now: T0,
                decay_per_day: 20,
                sweep_result: Ok(SweepOutcome::default()),
                registered,
                balances,
                active: BTreeMap::new(),
                stabled: BTreeMap::new(),
                supplies: BTreeMap::new(),
                accessories: BTreeMap::new(),
                graveyard: Vec::new(),
                next_pet_id: 1,
                next_accessory: "top_hat".to_owned(),
                marked_hatches: Vec::new(),
                marked_evolutions: Vec::new(),
                marked_deaths: Vec::new(),
                marked_refunds: Vec::new(),
            }
        }
    }

    impl FakeService {
        fn key(discord_id: i64, guild_id: Option<i64>) -> (i64, i64) {
            (discord_id, guild_id.unwrap_or_default())
        }

        fn active_pet(&self, discord_id: i64, guild_id: i64) -> Option<&Pet> {
            self.active.get(&(discord_id, guild_id))
        }

        fn stock(&self, discord_id: i64, guild_id: i64) -> BTreeMap<String, i64> {
            self.supplies
                .get(&(discord_id, guild_id))
                .cloned()
                .unwrap_or_default()
        }
    }

    impl PetCommandService for FakeService {
        fn decay_per_day(&self) -> i64 {
            self.decay_per_day
        }

        fn sweep(&mut self) -> Result<SweepOutcome, String> {
            self.sweep_result.clone()
        }

        fn mark_hatch_announced(&mut self, pet: &Pet) -> Result<(), String> {
            self.marked_hatches.push(pet.pet_id);
            Ok(())
        }

        fn mark_evolution_announced(&mut self, pet: &Pet) -> Result<(), String> {
            self.marked_evolutions.push(pet.pet_id);
            Ok(())
        }

        fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String> {
            self.marked_deaths.push(pet.pet_id);
            Ok(())
        }

        fn mark_refund_announced(&mut self, notice: &RefundNotice) -> Result<(), String> {
            self.marked_refunds
                .push((notice.guild_id, notice.week_key.clone()));
            Ok(())
        }

        fn adopt(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            name: &str,
            egg_tier: &str,
        ) -> ServiceResult<AdoptOutcome> {
            let key = Self::key(discord_id, guild_id);
            if !self.registered.contains(&key) {
                return ServiceResult::fail("Register with the league before adopting a pet.");
            }
            let living_count = usize::from(self.active.contains_key(&key))
                + self.stabled.get(&key).map_or(0, Vec::len);
            if living_count >= usize::try_from(MAX_LIVING_PETS).unwrap_or(5) {
                return ServiceResult::fail("Your stable is full.");
            }
            let fee = if egg_tier == "gilded" {
                20 + GILDED_EGG_PREMIUM
            } else {
                20
            };
            let balance = self.balances.entry(key).or_default();
            if *balance < fee {
                return ServiceResult::fail("Insufficient funds.");
            }
            *balance -= fee;
            let pet = Pet::new(PetBase {
                pet_id: self.next_pet_id,
                discord_id,
                guild_id: key.1,
                name: name.to_owned(),
                species: UNHATCHED_SPECIES.to_owned(),
                adopted_at: self.now,
                hatched_at: self.now + EGG_HATCH_SECONDS,
                adopt_fee: fee,
                last_fed_at: self.now + EGG_HATCH_SECONDS,
                hunger_at_last_fed: 100,
                times_fed: 0,
                feeds_today: 0,
                feed_date: None,
                week_consumed_jc: 0,
                week_key: None,
                prev_week_consumed_jc: 0,
                prev_week_key: None,
                pampered_until: None,
                accessory: None,
                dig_work_units: 0,
                dig_work_at: self.now + EGG_HATCH_SECONDS,
                aegis_used: 0,
                hatch_announced_at: None,
                died_at: None,
                death_cause: None,
                death_announced_at: None,
            });
            let mut pet = pet;
            pet.egg_tier = egg_tier.to_owned();
            pet.is_active = living_count == 0;
            pet.stabled_at = (living_count > 0).then_some(self.now);
            self.next_pet_id += 1;
            if pet.is_active {
                self.active.insert(key, pet.clone());
            } else {
                self.stabled.entry(key).or_default().push(pet.clone());
            }
            ServiceResult::ok(AdoptOutcome {
                pet,
                egg_tier: egg_tier.to_owned(),
                pity_active: false,
                upgraded: false,
            })
        }

        fn stable(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Vec<Pet>> {
            let key = Self::key(discord_id, guild_id);
            let mut pets = self
                .active
                .get(&key)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            pets.extend(self.stabled.get(&key).cloned().unwrap_or_default());
            ServiceResult::ok(pets)
        }

        fn activate(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            pet_id: i64,
        ) -> ServiceResult<cama_db::pet_repository::ActivatePetOutcome> {
            let key = Self::key(discord_id, guild_id);
            let Some(position) = self
                .stabled
                .get(&key)
                .and_then(|pets| pets.iter().position(|pet| pet.pet_id == pet_id))
            else {
                return ServiceResult::fail("That pet is not in your stable.");
            };
            let balance = self.balances.entry(key).or_default();
            if *balance < PET_SWITCH_COST {
                return ServiceResult::fail("Insufficient funds.");
            }
            let Some(mut previous_pet) = self.active.remove(&key) else {
                return ServiceResult::fail("You have no active pet.");
            };
            *balance -= PET_SWITCH_COST;
            let mut active_pet = self
                .stabled
                .get_mut(&key)
                .expect("stable entry")
                .remove(position);
            previous_pet.is_active = false;
            previous_pet.stabled_at = Some(self.now);
            active_pet.is_active = true;
            active_pet.stabled_at = None;
            self.stabled
                .entry(key)
                .or_default()
                .push(previous_pet.clone());
            self.active.insert(key, active_pet.clone());
            ServiceResult::ok(cama_db::pet_repository::ActivatePetOutcome {
                previous_pet,
                active_pet,
                cost: PET_SWITCH_COST,
                next_switch_at: self.now + PET_SWITCH_COOLDOWN_SECONDS,
            })
        }

        fn upgrade(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            pet_id: i64,
        ) -> ServiceResult<Pet> {
            let key = Self::key(discord_id, guild_id);
            let pet = self
                .active
                .get_mut(&key)
                .filter(|pet| pet.pet_id == pet_id)
                .or_else(|| {
                    self.stabled
                        .get_mut(&key)
                        .and_then(|pets| pets.iter_mut().find(|pet| pet.pet_id == pet_id))
                });
            let Some(pet) = pet else {
                return ServiceResult::fail("You have no living pet.");
            };
            if pet.species != UNHATCHED_SPECIES || pet.egg_tier != "standard" {
                return ServiceResult::fail("Only an unhatched egg can be upgraded.");
            }
            let balance = self.balances.entry(key).or_default();
            if *balance < GILDED_EGG_PREMIUM {
                return ServiceResult::fail("Insufficient funds.");
            }
            *balance -= GILDED_EGG_PREMIUM;
            pet.egg_tier = "gilded".to_owned();
            pet.adopt_fee += GILDED_EGG_PREMIUM;
            ServiceResult::ok(pet.clone())
        }

        fn sacrifice_preview(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> ServiceResult<SacrificePreview> {
            let key = Self::key(discord_id, guild_id);
            let Some(pet) = self.active.get(&key).cloned() else {
                return ServiceResult::fail("You have no living pet to sacrifice.");
            };
            if self.now < pet.hatched_at {
                return ServiceResult::fail("You can't sacrifice a mystery. Let it hatch first.");
            }
            let tier = get_species(&pet.species).tier;
            let was_adult = pet.stage(self.now) == PetStage::Adult;
            ServiceResult::ok(SacrificePreview {
                pet,
                tier,
                was_adult,
                fee: 50,
                weights: cama_domain::pet::sacrifice_tier_weights(tier, was_adult),
            })
        }

        fn sacrifice(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            name: &str,
        ) -> ServiceResult<SacrificeOutcome> {
            let preview = match self.sacrifice_preview(discord_id, guild_id) {
                ServiceResult::Success(preview) => preview,
                ServiceResult::Failure { error, error_code } => {
                    return ServiceResult::Failure { error, error_code };
                }
            };
            let key = Self::key(discord_id, guild_id);
            let balance = self.balances.entry(key).or_default();
            if *balance < preview.fee {
                return ServiceResult::fail("Insufficient funds.");
            }
            *balance -= preview.fee;
            let mut dead_pet = preview.pet;
            dead_pet.died_at = Some(self.now);
            dead_pet.death_cause = Some("sacrifice".to_owned());
            self.graveyard.push(dead_pet.clone());
            let mut new_pet = Pet::new(PetBase {
                pet_id: self.next_pet_id,
                discord_id,
                guild_id: key.1,
                name: name.to_owned(),
                species: "rama".to_owned(),
                adopted_at: self.now,
                hatched_at: self.now + EGG_HATCH_SECONDS,
                adopt_fee: preview.fee,
                last_fed_at: self.now + EGG_HATCH_SECONDS,
                hunger_at_last_fed: 100,
                times_fed: 0,
                feeds_today: 0,
                feed_date: None,
                week_consumed_jc: 0,
                week_key: None,
                prev_week_consumed_jc: 0,
                prev_week_key: None,
                pampered_until: None,
                accessory: None,
                dig_work_units: 0,
                dig_work_at: self.now + EGG_HATCH_SECONDS,
                aegis_used: 0,
                hatch_announced_at: None,
                died_at: None,
                death_cause: None,
                death_announced_at: None,
            });
            new_pet.evolution_started_at = Some(new_pet.hatched_at);
            new_pet.evolution_due_at = Some(
                new_pet
                    .hatched_at
                    .saturating_add(cama_domain::pet::ADULT_AGE_SECONDS),
            );
            self.next_pet_id += 1;
            self.active.insert(key, new_pet.clone());
            ServiceResult::ok(SacrificeOutcome {
                dead_pet,
                new_pet,
                fee: preview.fee,
                tier: preview.tier,
                was_adult: preview.was_adult,
            })
        }

        fn status(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<PetStatus> {
            let key = Self::key(discord_id, guild_id);
            let supplies = self.supplies.get(&key).cloned().unwrap_or_default();
            let Some(mut pet) = self.active.get(&key).cloned() else {
                return ServiceResult::ok(PetStatus {
                    supplies: Some(supplies),
                    ..empty_status()
                });
            };
            if self.now >= pet.hatched_at && pet.species == UNHATCHED_SPECIES {
                pet.species = "common_cama".to_owned();
                self.active.insert(key, pet.clone());
            }
            let stage = pet.stage(self.now);
            let hunger = pet.current_hunger(self.now, self.decay_per_day);
            ServiceResult::ok(PetStatus {
                pet: Some(pet.clone()),
                hunger,
                stage: Some(stage),
                mood: Some(pet.mood(self.now, self.decay_per_day)),
                age_seconds: pet.age_seconds(self.now),
                supplies: Some(supplies),
                last_dead: None,
                dig_work_units: pet.dig_work_units,
                dig_work_rate: if stage == PetStage::Egg { 0 } else { 8 },
                evolution_hint: None,
            })
        }

        fn next_adoption_fee(&mut self, _discord_id: i64, _guild_id: Option<i64>) -> i64 {
            20
        }

        fn balance(&mut self, discord_id: i64, guild_id: Option<i64>) -> Option<i64> {
            self.balances.get(&Self::key(discord_id, guild_id)).copied()
        }

        fn buy(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            item_id: &str,
            quantity: i64,
        ) -> ServiceResult<BuyOutcome> {
            let key = Self::key(discord_id, guild_id);
            let Some(food) = food_by_id(item_id) else {
                return ServiceResult::fail("Unknown item.");
            };
            let unit_cost = self
                .active
                .get(&key)
                .filter(|pet| self.now >= pet.hatched_at)
                .map_or(food.cost, |pet| {
                    food_cost(&pet.species, item_id).unwrap_or(food.cost)
                });
            let total_cost = unit_cost * quantity;
            let balance = self.balances.entry(key).or_default();
            if *balance < total_cost {
                return ServiceResult::fail("Insufficient funds.");
            }
            *balance -= total_cost;
            let stock = self.supplies.entry(key).or_default();
            let new_qty = stock.entry(item_id.to_owned()).or_default();
            *new_qty += quantity;
            ServiceResult::ok(BuyOutcome {
                item_id: item_id.to_owned(),
                qty: quantity,
                total_cost,
                new_qty: Some(*new_qty),
                pampered_until: None,
                pet: None,
            })
        }

        fn feed(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            item_id: &str,
        ) -> ServiceResult<FeedOutcome> {
            let key = Self::key(discord_id, guild_id);
            let Some(food) = food_by_id(item_id) else {
                return ServiceResult::fail("Unknown item.");
            };
            let Some(pet) = self.active.get_mut(&key) else {
                return ServiceResult::fail("You have no pet.");
            };
            if self.now < pet.hatched_at {
                return ServiceResult::fail("Your pet is still an egg.");
            }
            if pet.species == UNHATCHED_SPECIES {
                pet.species = "common_cama".to_owned();
            }
            let stock = self.supplies.entry(key).or_default();
            let quantity = stock.entry(item_id.to_owned()).or_default();
            if *quantity <= 0 {
                return ServiceResult::fail("You have none of that food.");
            }
            *quantity -= 1;
            let old_hunger = pet.current_hunger(self.now, self.decay_per_day);
            let new_hunger = (old_hunger + food.restore).min(100);
            pet.last_fed_at = self.now;
            pet.hunger_at_last_fed = new_hunger;
            pet.times_fed += 1;
            pet.feeds_today += 1;
            ServiceResult::ok(FeedOutcome {
                pet: pet.clone(),
                item_id: item_id.to_owned(),
                old_hunger,
                new_hunger,
                remaining_qty: *quantity,
                feeds_left_today: (FEED_CAP_PER_DAY - pet.feeds_today).max(0),
                spat: false,
            })
        }

        fn roll_trinket(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
        ) -> ServiceResult<TrinketOutcome> {
            let key = Self::key(discord_id, guild_id);
            let Some(pet) = self.active.get_mut(&key) else {
                return ServiceResult::fail("You have no pet.");
            };
            if self.now < pet.hatched_at {
                return ServiceResult::fail("Your pet is still an egg.");
            }
            if pet.species == UNHATCHED_SPECIES {
                pet.species = "common_cama".to_owned();
            }
            let owned = self.accessories.entry(key).or_default();
            let duplicate = !owned.insert(self.next_accessory.clone());
            pet.accessory = Some(self.next_accessory.clone());
            ServiceResult::ok(TrinketOutcome {
                accessory_id: self.next_accessory.clone(),
                duplicate,
                net_cost: if duplicate { 15 } else { 25 },
                owned_count: i64::try_from(owned.len()).unwrap_or_default(),
            })
        }

        fn wear_trinket(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            accessory_id: &str,
        ) -> ServiceResult<String> {
            let key = Self::key(discord_id, guild_id);
            if !self
                .accessories
                .get(&key)
                .is_some_and(|owned| owned.contains(accessory_id))
            {
                return ServiceResult::fail("You do not own that trinket.");
            }
            let Some(pet) = self.active.get_mut(&key) else {
                return ServiceResult::fail("You have no pet.");
            };
            pet.accessory = Some(accessory_id.to_owned());
            ServiceResult::ok(accessory_id.to_owned())
        }

        fn owned_trinkets(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<String> {
            self.accessories
                .get(&Self::key(discord_id, guild_id))
                .map_or_else(Vec::new, |owned| owned.iter().cloned().collect())
        }

        fn rename(
            &mut self,
            discord_id: i64,
            guild_id: Option<i64>,
            name: &str,
        ) -> ServiceResult<Pet> {
            let Some(pet) = self.active.get_mut(&Self::key(discord_id, guild_id)) else {
                return ServiceResult::fail("You have no pet.");
            };
            pet.name = name.to_owned();
            ServiceResult::ok(pet.clone())
        }

        fn graveyard(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<Pet> {
            self.graveyard
                .iter()
                .filter(|pet| pet.discord_id == discord_id && Some(pet.guild_id) == guild_id)
                .cloned()
                .collect()
        }

        fn leaderboard(&mut self, guild_id: Option<i64>) -> Vec<Pet> {
            self.active
                .values()
                .filter(|pet| Some(pet.guild_id) == guild_id && self.now >= pet.hatched_at)
                .cloned()
                .collect()
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeFlavor {
        responses: BTreeMap<PetFlavorEvent, String>,
        calls: Vec<PetFlavorEvent>,
        trace: Option<Rc<RefCell<Vec<&'static str>>>>,
    }

    impl PetFlavorPort for FakeFlavor {
        fn generate(
            &mut self,
            event: PetFlavorEvent,
            _pet: &Pet,
            _status: Option<&PetStatus>,
        ) -> Option<String> {
            self.calls.push(event);
            if let Some(trace) = self.trace.as_ref() {
                trace.borrow_mut().push("flavor");
            }
            self.responses.get(&event).cloned()
        }
    }

    struct TraceInteraction {
        trace: Rc<RefCell<Vec<&'static str>>>,
        events: Vec<InteractionEvent>,
    }

    impl InteractionPort for TraceInteraction {
        fn send_immediate(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
            self.trace.borrow_mut().push("immediate");
            self.events.push(InteractionEvent::Immediate(payload));
            Ok(())
        }

        fn defer(&mut self, visibility: Visibility) -> bool {
            self.trace.borrow_mut().push("defer");
            self.events.push(InteractionEvent::Deferred(visibility));
            true
        }

        fn send_followup(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
            self.trace.borrow_mut().push("followup");
            self.events.push(InteractionEvent::Followup(payload));
            Ok(())
        }

        fn edit_original(&mut self, payload: ResponsePayload) -> Result<(), TransportError> {
            self.trace.borrow_mut().push("edit");
            self.events.push(InteractionEvent::EditOriginal(payload));
            Ok(())
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeReminder {
        enabled: bool,
        schedules: Vec<(i64, i64, i64, String, bool)>,
        cancellations: Vec<(i64, i64)>,
    }

    impl PetReminderPort for FakeReminder {
        fn pet_enabled(&mut self, _discord_id: i64, _guild_id: i64) -> bool {
            self.enabled
        }

        fn schedule_pet_reminder(
            &mut self,
            discord_id: i64,
            guild_id: i64,
            crossing: i64,
            pet_name: &str,
            preference_enabled: bool,
        ) {
            self.schedules.push((
                discord_id,
                guild_id,
                crossing,
                pet_name.to_owned(),
                preference_enabled,
            ));
        }

        fn cancel_pet_reminder(&mut self, discord_id: i64, guild_id: i64) {
            self.cancellations.push((discord_id, guild_id));
        }
    }

    #[derive(Clone, Debug)]
    struct FakeRateLimiter {
        decision: RateLimitDecision,
    }

    impl Default for FakeRateLimiter {
        fn default() -> Self {
            Self {
                decision: RateLimitDecision {
                    allowed: true,
                    retry_after_seconds: 0,
                },
            }
        }
    }

    impl RateLimitPort for FakeRateLimiter {
        fn check(&mut self, _scope: &str, _guild_id: i64, _user_id: i64) -> RateLimitDecision {
            self.decision
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeDiscord {
        channels: BTreeSet<(i64, i64)>,
        resolution_requests: Vec<(i64, i64)>,
        channel_attempts: Vec<(i64, ResponsePayload)>,
        channel_failures: VecDeque<Option<DeliveryError>>,
        dms: Vec<(i64, ResponsePayload)>,
    }

    impl FakeDiscord {
        fn with_channel(mut self, guild_id: i64, channel_id: i64) -> Self {
            self.channels.insert((guild_id, channel_id));
            self
        }
    }

    impl DiscordDeliveryPort for FakeDiscord {
        fn resolve_text_channel(&mut self, guild_id: i64, channel_id: i64) -> Option<i64> {
            self.resolution_requests.push((guild_id, channel_id));
            self.channels
                .contains(&(guild_id, channel_id))
                .then_some(channel_id)
        }

        fn send_channel(
            &mut self,
            channel_id: i64,
            payload: ResponsePayload,
        ) -> Result<(), DeliveryError> {
            self.channel_attempts.push((channel_id, payload));
            match self.channel_failures.pop_front().flatten() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn send_dm(
            &mut self,
            discord_id: i64,
            payload: ResponsePayload,
        ) -> Result<(), DeliveryError> {
            self.dms.push((discord_id, payload));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeWarningSource {
        warnings: VecDeque<Option<(i64, String)>>,
    }

    impl PetWarningSource for FakeWarningSource {
        fn warning_crossing(&mut self, _discord_id: i64, _guild_id: i64) -> Option<(i64, String)> {
            self.warnings.pop_front().flatten()
        }
    }

    type TestApp = PetCommands<FakeService, FakeFlavor, FakeReminder, FakeRateLimiter>;

    fn make_app(service: FakeService) -> TestApp {
        PetCommands::new(
            service,
            FakeFlavor::default(),
            None,
            FakeRateLimiter::default(),
            PetCommandConfig {
                pet_channel_id: Some(42),
            },
        )
    }

    fn context(user_id: i64, now: i64) -> InteractionContext {
        InteractionContext {
            user_id,
            display_name: if user_id == OWNER { "Owner" } else { "Viewer" }.to_owned(),
            guild_id: Some(GUILD),
            now,
        }
    }

    fn adopt_via_handler(app: &mut TestApp, name: &str, egg_tier: &str) -> InteractionRecorder {
        let mut interaction = InteractionRecorder::default();
        app.adopt(
            &mut interaction,
            &context(OWNER, app.service().now),
            name,
            egg_tier,
        );
        interaction
    }

    fn payload_content(interaction: &InteractionRecorder) -> &str {
        interaction
            .last_payload()
            .and_then(|payload| payload.content.as_deref())
            .expect("expected response content")
    }

    fn payload_embed(interaction: &InteractionRecorder) -> &Embed {
        interaction
            .last_payload()
            .and_then(|payload| payload.embed.as_ref())
            .expect("expected response embed")
    }

    #[test]
    fn test_eaten_death_retry_uses_the_exact_durable_outcome() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("eaten".to_owned());
        let eating_outcome = BTreeMap::from([
            ("reward".to_owned(), 888),
            ("penalty_games_added".to_owned(), 4),
            ("penalty_games_remaining".to_owned(), 7),
            ("new_balance".to_owned(), 1_888),
        ]);
        let mut activated = make_pet();
        activated.pet_id = 2;
        activated.name = "Backup".to_owned();
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet: pet.clone(),
                eating_outcome: Some(eating_outcome),
                activated_pet: Some(activated),
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        let embed = discord.channel_attempts[0]
            .1
            .embed
            .as_ref()
            .expect("death embed");
        let copy = format!("{}\n{}", embed.title, embed.description);
        assert!(copy.contains("888"));
        assert!(copy.contains('4'));
        assert!(copy.contains("7 remaining"));
        assert!(copy.contains("1,888"));
        assert_eq!(
            embed.field_value("🏡 New active cama"),
            Some("**Backup** was automatically activated from the stable.")
        );
        assert_eq!(app.service().marked_deaths, vec![pet.pet_id]);
    }

    #[test]
    fn test_sweep_skips_a_death_being_delivered_by_the_command() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("eaten".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet: pet.clone(),
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        app.set_direct_death_delivery(pet.pet_id, 1);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        assert!(discord.channel_attempts.is_empty());
        assert!(app.service().marked_deaths.is_empty());
    }

    #[test]
    fn test_evolution_posts_before_death_and_marks_independently() {
        let mut evolved = make_pet();
        evolved.evolved_at = Some(T0 + 8 * DAY);
        evolved.evolution_calling = Some("oracle".to_owned());
        evolved.evolution_primary = Some("fortune".to_owned());
        evolved.evolution_secondary = Some("wisdom".to_owned());
        let mut dead = make_pet();
        dead.pet_id = 2;
        dead.died_at = Some(T0 + 9 * DAY);
        dead.death_cause = Some("starvation".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            evolutions: vec![EvolutionNotice {
                pet: evolved.clone(),
            }],
            deaths: vec![DeathNotice {
                pet: dead.clone(),
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        assert_eq!(discord.channel_attempts.len(), 2);
        assert!(
            discord.channel_attempts[0]
                .1
                .embed
                .as_ref()
                .expect("evolution embed")
                .title
                .contains("evolved")
        );
        assert_eq!(app.service().marked_evolutions, vec![evolved.pet_id]);
        assert_eq!(app.service().marked_deaths, vec![dead.pet_id]);
    }

    #[test]
    fn test_hatch_posts_to_channel_and_marks() {
        let pet = make_pet();
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            hatches: vec![HatchNotice { pet: pet.clone() }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        app.reminders = Some(FakeReminder {
            enabled: true,
            ..FakeReminder::default()
        });
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        assert_eq!(discord.channel_attempts.len(), 1);
        assert_eq!(app.service().marked_hatches, vec![pet.pet_id]);
        assert_eq!(app.reminders().expect("reminders").schedules.len(), 1);
    }

    #[test]
    fn test_hatch_announcement_includes_generated_flavor() {
        let pet = make_pet();
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            hatches: vec![HatchNotice { pet }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        app.flavor.responses.insert(
            PetFlavorEvent::Hatched,
            "New hooves online. The snack economy will never recover.".to_owned(),
        );
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        let embed = discord.channel_attempts[0]
            .1
            .embed
            .as_ref()
            .expect("hatch embed");
        assert_eq!(
            embed.field_value("💬 Cama chatter"),
            Some("New hooves online. The snack economy will never recover.")
        );
    }

    #[test]
    fn production_hatch_evolution_and_death_embeds_match_the_python_presentation() {
        let mut legendary = make_pet();
        legendary.species = "sunspun_cama".to_owned();
        let hatch = build_hatch_embed(&legendary);
        assert_eq!(hatch.color, EmbedColor::Custom(0xE9_1E_63));
        assert!(hatch.description.contains("🎆 UNBELIEVABLE — it's the"));
        assert!(hatch.description.contains("Sunspun Cama"));
        assert!(hatch.description.contains("Sun-warm threads"));
        assert_eq!(hatch.image.as_deref(), Some("attachment://pet-1.png"));

        legendary.evolution_calling = Some("oracle".to_owned());
        legendary.evolution_primary = Some("fortune".to_owned());
        legendary.evolution_secondary = Some("wisdom".to_owned());
        let evolution = build_evolution_embed(&legendary);
        assert_eq!(evolution.title, "🔮 Blep evolved — Oracle!");
        assert_eq!(
            evolution.color,
            EmbedColor::Custom(calling_profile(PetCalling::Oracle).color)
        );
        assert!(evolution.description.contains("👑 Sunspun Cama"));
        assert!(evolution.description.contains("**Fortune + Wisdom**"));
        assert!(evolution.description.contains("Reads tomorrow in patterns"));
        assert_eq!(evolution.image.as_deref(), Some("attachment://pet-1.png"));

        legendary.died_at = Some(legendary.hatched_at + 9 * DAY);
        legendary.death_cause = Some("starvation".to_owned());
        let death = build_death_embed(&legendary);
        assert!(death.description.contains("👑 Sunspun Cama · 🔮 Oracle"));
        assert!(death.description.contains("starved, 9 days old"));
        assert!(
            death
                .description
                .contains(&format!("<t:{}:f>", legendary.hatched_at + 9 * DAY))
        );
        assert_eq!(
            death.image.as_deref(),
            Some("attachment://pet-tombstone-1.png")
        );
    }

    #[test]
    fn dead_status_embed_preserves_python_memorial_copy_and_tombstone() {
        let mut dead = make_pet();
        dead.species = "sunspun_cama".to_owned();
        dead.evolution_calling = Some("oracle".to_owned());
        dead.died_at = Some(dead.hatched_at + 9 * DAY);
        dead.death_cause = Some("eaten".to_owned());
        let status = PetStatus {
            pet: None,
            hunger: 0,
            stage: None,
            mood: None,
            age_seconds: 0,
            supplies: None,
            last_dead: Some(dead.clone()),
            dig_work_units: 0,
            dig_work_rate: 0,
            evolution_hint: None,
        };

        let embed = build_status_embed(&status, 20, dead.died_at.unwrap(), "Owner", 40);

        assert_eq!(embed.title, "In memoriam: Blep");
        assert_eq!(
            embed.description,
            format!(
                "👑 Sunspun Cama · 🔮 Oracle · was eaten, 9 days old — <t:{}:R>\n\n_{}_\n\nF.\n\nAdopt again with `/pet adopt` — 40 {JOPACOIN_EMOTE}",
                dead.died_at.unwrap(),
                get_species("sunspun_cama").blurb,
            )
        );
        assert_eq!(
            embed.image.as_deref(),
            Some("attachment://pet-tombstone-1.png")
        );
    }

    #[test]
    fn test_no_channel_still_marks_announced() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("starvation".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet: pet.clone(),
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default();

        app.sweep_once(&mut discord);

        assert_eq!(app.service().marked_deaths, vec![pet.pet_id]);
    }

    #[test]
    fn test_death_skips_flavor_when_no_delivery_surface() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("starvation".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet: pet.clone(),
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        app.reminders = Some(FakeReminder::default());
        app.flavor
            .responses
            .insert(PetFlavorEvent::Died, "Unused memorial.".to_owned());
        let mut discord = FakeDiscord::default();

        app.sweep_once(&mut discord);

        assert!(app.flavor().calls.is_empty());
        assert_eq!(app.service().marked_deaths, vec![pet.pet_id]);
    }

    #[test]
    fn test_death_announcement_includes_gentle_generated_flavor() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("starvation".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet,
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        app.flavor.responses.insert(
            PetFlavorEvent::Died,
            "The great pasture saved the warmest sunbeam for Blep.".to_owned(),
        );
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        assert_eq!(
            discord.channel_attempts[0]
                .1
                .embed
                .as_ref()
                .and_then(|embed| embed.field_value("💬 Cama chatter")),
            Some("The great pasture saved the warmest sunbeam for Blep.")
        );
    }

    #[test]
    fn test_forbidden_marks_announced() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("starvation".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet: pet.clone(),
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);
        discord
            .channel_failures
            .push_back(Some(DeliveryError::Forbidden));

        app.sweep_once(&mut discord);

        assert_eq!(app.service().marked_deaths, vec![pet.pet_id]);
    }

    #[test]
    fn test_transient_error_leaves_unmarked_for_retry() {
        let mut pet = make_pet();
        pet.died_at = Some(T0 + 9 * DAY);
        pet.death_cause = Some("starvation".to_owned());
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![DeathNotice {
                pet,
                eating_outcome: None,
                activated_pet: None,
            }],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);
        discord
            .channel_failures
            .push_back(Some(DeliveryError::Transient));

        app.sweep_once(&mut discord);

        assert!(app.service().marked_deaths.is_empty());
    }

    #[test]
    fn test_one_bad_notice_does_not_block_others() {
        let mut bad = make_pet();
        bad.died_at = Some(T0 + 9 * DAY);
        bad.death_cause = Some("starvation".to_owned());
        let mut good = bad.clone();
        good.pet_id = 2;
        good.discord_id = 222;
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            deaths: vec![
                DeathNotice {
                    pet: bad,
                    eating_outcome: None,
                    activated_pet: None,
                },
                DeathNotice {
                    pet: good.clone(),
                    eating_outcome: None,
                    activated_pet: None,
                },
            ],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);
        discord
            .channel_failures
            .push_back(Some(DeliveryError::Transient));
        discord.channel_failures.push_back(None);

        app.sweep_once(&mut discord);

        assert_eq!(discord.channel_attempts.len(), 2);
        assert_eq!(app.service().marked_deaths, vec![good.pet_id]);
    }

    fn refund_notice() -> RefundNotice {
        RefundNotice {
            guild_id: GUILD,
            week_key: "2026-W30".to_owned(),
            payouts: vec![RefundPayout {
                discord_id: 111,
                consumed_jc: 20,
                multiplier_pct: 150,
                amount: 30,
            }],
            total_paid: 30,
            scaled_down: false,
        }
    }

    #[test]
    fn test_refund_summary_posts() {
        let notice = refund_notice();
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            refunds: vec![notice.clone()],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        assert!(
            discord.channel_attempts[0]
                .1
                .embed
                .as_ref()
                .expect("refund embed")
                .description
                .contains("150%")
        );
        assert_eq!(
            app.service().marked_refunds,
            vec![(GUILD, "2026-W30".to_owned())]
        );
    }

    #[test]
    fn test_transient_refund_error_leaves_unmarked_for_retry() {
        let mut service = FakeService::default();
        service.sweep_result = Ok(SweepOutcome {
            refunds: vec![refund_notice()],
            ..SweepOutcome::default()
        });
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);
        discord
            .channel_failures
            .push_back(Some(DeliveryError::Transient));

        app.sweep_once(&mut discord);

        assert!(app.service().marked_refunds.is_empty());
    }

    #[test]
    fn test_sweep_service_failure_is_contained() {
        let mut service = FakeService::default();
        service.sweep_result = Err("db locked".to_owned());
        let mut app = make_app(service);
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);

        app.sweep_once(&mut discord);

        assert!(discord.channel_attempts.is_empty());
    }

    #[test]
    fn test_setup_skips_when_feature_disabled() {
        assert_eq!(setup_decision(false, false), Ok(false));
    }

    #[test]
    fn test_setup_loads_cog_when_service_present() {
        assert_eq!(setup_decision(true, true), Ok(true));
    }

    #[test]
    fn test_container_gates_service_on_channel_id() {
        assert_eq!(
            PetFeatureComponents::for_channel(None),
            PetFeatureComponents::default()
        );
        let components = PetFeatureComponents::for_channel(Some(123_456));
        assert!(components.pet_service);
        assert!(components.evolution_service);
        assert!(components.flavor_service);
        assert!(!components.flavor_ai_service);
        assert!(components.shares_evolution_service);
        assert!(components.shares_guild_config_repo);
        assert!(components.shares_economy_ledger_repo);
        assert!(components.shares_player_repo);
    }

    #[test]
    fn test_adopt_happy_path_sends_egg_embed() {
        let mut app = make_app(FakeService::default());

        let interaction = adopt_via_handler(&mut app, "Blep", "standard");

        assert!(
            payload_embed(&interaction)
                .title
                .to_lowercase()
                .contains("mysterious egg")
        );
        assert_eq!(
            app.service()
                .active_pet(OWNER, GUILD)
                .map(|pet| pet.name.as_str()),
            Some("Blep")
        );
    }

    #[test]
    fn test_adopt_does_not_arm_warning_before_hatch() {
        let mut app = make_app(FakeService::default());
        app.reminders = Some(FakeReminder {
            enabled: true,
            ..FakeReminder::default()
        });

        let _ = adopt_via_handler(&mut app, "Blep", "standard");

        assert!(app.reminders().expect("reminders").schedules.is_empty());
    }

    #[test]
    fn test_second_gilded_adoption_creates_a_stabled_egg() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");

        let interaction = adopt_via_handler(&mut app, "Goldie", "gilded");

        let embed = payload_embed(&interaction);
        assert!(embed.title.to_lowercase().contains("gilded"));
        assert!(embed.description.contains("waiting safely in the stable"));
        let active = app
            .service()
            .active_pet(OWNER, GUILD)
            .expect("original active pet");
        assert_eq!(active.name, "Blep");
        let stabled = &app.service().stabled[&(OWNER, GUILD)][0];
        assert_eq!(stabled.name, "Goldie");
        assert_eq!(stabled.egg_tier, "gilded");
        assert!(!stabled.is_active);
        assert_eq!(stabled.adopt_fee, 250);
        assert_eq!(app.service().balances[&(OWNER, GUILD)], 730);
    }

    #[test]
    fn test_sixth_adoption_reports_a_full_stable_without_a_charge() {
        let mut app = make_app(FakeService::default());
        for name in ["One", "Two", "Three", "Four", "Five"] {
            let _interaction = adopt_via_handler(&mut app, name, "standard");
        }

        let interaction = adopt_via_handler(&mut app, "Six", "standard");

        assert!(payload_content(&interaction).starts_with("❌"));
        assert_eq!(app.service().balances[&(OWNER, GUILD)], 900);
        assert_eq!(app.service().stabled[&(OWNER, GUILD)].len(), 4);
    }

    #[test]
    fn test_adoption_embed_includes_generated_egg_flavor() {
        let mut app = make_app(FakeService::default());
        app.flavor.responses.insert(
            PetFlavorEvent::Adopted,
            "The egg has accepted the lease and requests one sunbeam.".to_owned(),
        );

        let interaction = adopt_via_handler(&mut app, "Blep", "standard");

        assert_eq!(
            payload_embed(&interaction).field_value("💬 Cama chatter"),
            Some("The egg has accepted the lease and requests one sunbeam.")
        );
    }

    #[test]
    fn test_adopt_unregistered_reports_error() {
        let mut app = make_app(FakeService::default());
        let mut interaction = InteractionRecorder::default();

        app.adopt(&mut interaction, &context(999, T0), "Ghost", "standard");

        assert!(payload_content(&interaction).starts_with("❌"));
    }

    #[test]
    fn test_status_own_pet_returns_view_and_art() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let mut interaction = InteractionRecorder::default();

        app.status(&mut interaction, &context(OWNER, T0), OWNER, "Owner", false);

        let payload = interaction.last_payload().expect("status payload");
        assert!(payload.embed.is_some());
        assert!(payload.view.is_some());
        assert!(payload.attachment.is_some());
        assert_eq!(payload.visibility, Visibility::Ephemeral);
    }

    #[test]
    fn test_status_embed_includes_generated_pet_chatter() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        app.flavor.responses.insert(
            PetFlavorEvent::Status,
            "The egg is practicing its mysterious little wobble.".to_owned(),
        );
        let mut interaction = InteractionRecorder::default();

        app.status(&mut interaction, &context(OWNER, T0), OWNER, "Owner", false);

        assert_eq!(
            payload_embed(&interaction).field_value("💬 Cama chatter"),
            Some("The egg is practicing its mysterious little wobble.")
        );
    }

    #[test]
    fn test_status_other_user_has_no_buttons() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let mut interaction = InteractionRecorder::default();

        app.status(&mut interaction, &context(555, T0), OWNER, "Owner", false);

        assert!(
            interaction
                .last_payload()
                .expect("status payload")
                .view
                .is_none()
        );
    }

    #[test]
    fn test_status_rate_limit_stops_uncached_target_fanout() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        app.rate_limiter.decision = RateLimitDecision {
            allowed: false,
            retry_after_seconds: 9,
        };
        let mut interaction = InteractionRecorder::default();

        app.status(&mut interaction, &context(OWNER, T0), OWNER, "Owner", false);

        assert_eq!(interaction.events.len(), 1);
        assert_eq!(
            interaction.last_payload(),
            Some(&ResponsePayload::content(
                "⏳ Please wait 9s.",
                Visibility::Ephemeral
            ))
        );
    }

    #[test]
    fn test_buy_and_feed_flow_through_handlers() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let mut buy_interaction = InteractionRecorder::default();
        app.buy(&mut buy_interaction, &context(OWNER, T0), "tango", 3);
        assert!(payload_content(&buy_interaction).contains("Bought 3× Tango"));
        let feeding_time = T0 + EGG_HATCH_SECONDS + DAY;
        app.service_mut().now = feeding_time;
        let mut feed_interaction = InteractionRecorder::default();

        app.feed(
            &mut feed_interaction,
            &context(OWNER, feeding_time),
            "tango",
        );

        let content = payload_content(&feed_interaction);
        assert!(content.contains("munches"));
        assert!(content.contains("2 left"));
    }

    #[test]
    fn test_feed_keeps_facts_and_appends_generated_flavor() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let mut buy_interaction = InteractionRecorder::default();
        app.buy(&mut buy_interaction, &context(OWNER, T0), "tango", 1);
        let feeding_time = T0 + EGG_HATCH_SECONDS + DAY;
        app.service_mut().now = feeding_time;
        app.flavor.responses.insert(
            PetFlavorEvent::Fed,
            "Snack acquired. Tail-wag protocol engaged.".to_owned(),
        );
        let mut interaction = InteractionRecorder::default();

        app.feed(&mut interaction, &context(OWNER, feeding_time), "tango");

        let content = payload_content(&interaction);
        assert!(content.contains("munches the Tango"));
        assert!(content.contains("Fullness "));
        assert!(content.contains("→ **100**"));
        assert!(content.contains("Snack acquired. Tail-wag protocol engaged."));
    }

    #[test]
    fn test_feed_egg_rejected_via_handler() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let mut buy_interaction = InteractionRecorder::default();
        app.buy(&mut buy_interaction, &context(OWNER, T0), "tango", 1);
        let mut interaction = InteractionRecorder::default();

        app.feed(&mut interaction, &context(OWNER, T0), "tango");

        assert!(payload_content(&interaction).starts_with("❌"));
    }

    #[test]
    fn test_trinket_roll_via_handler() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let hatched = T0 + EGG_HATCH_SECONDS + 3_600;
        app.service_mut().now = hatched;
        app.service_mut().next_accessory = "top_hat".to_owned();
        let mut interaction = InteractionRecorder::default();

        app.trinket(&mut interaction, &context(OWNER, hatched), None);

        let content = payload_content(&interaction);
        assert!(content.contains("Top Hat"));
        assert!(content.contains("Equipped"));
        assert_eq!(
            app.service()
                .active_pet(OWNER, GUILD)
                .and_then(|pet| pet.accessory.as_deref()),
            Some("top_hat")
        );
    }

    #[test]
    fn test_trinket_wear_branch_via_handler() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let hatched = T0 + EGG_HATCH_SECONDS + 3_600;
        app.service_mut().now = hatched;
        for accessory in ["red_bow", "top_hat"] {
            app.service_mut().next_accessory = accessory.to_owned();
            let mut roll = InteractionRecorder::default();
            app.trinket(&mut roll, &context(OWNER, hatched), None);
        }
        let mut interaction = InteractionRecorder::default();

        app.trinket(&mut interaction, &context(OWNER, hatched), Some("red_bow"));

        assert!(payload_content(&interaction).contains("Now wearing"));
        assert!(payload_content(&interaction).contains("Red Bow"));
        assert_eq!(
            app.service()
                .active_pet(OWNER, GUILD)
                .and_then(|pet| pet.accessory.as_deref()),
            Some("red_bow")
        );
    }

    #[test]
    fn test_trinket_rejected_for_egg() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let mut interaction = InteractionRecorder::default();

        app.trinket(&mut interaction, &context(OWNER, T0), None);

        assert!(payload_content(&interaction).starts_with("❌"));
    }

    #[test]
    fn test_trinket_autocomplete_lists_owned_only() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let hatched = T0 + EGG_HATCH_SECONDS + 3_600;
        app.service_mut().now = hatched;
        app.service_mut().next_accessory = "wool_scarf".to_owned();
        let mut roll = InteractionRecorder::default();
        app.trinket(&mut roll, &context(OWNER, hatched), None);

        let choices = app.trinket_autocomplete(&context(OWNER, hatched), "");
        let no_choices = app.trinket_autocomplete(&context(OWNER, hatched), "top hat");

        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.value.as_str())
                .collect::<Vec<_>>(),
            vec!["wool_scarf"]
        );
        assert!(no_choices.is_empty());
    }

    #[test]
    fn test_shop_rename_graveyard_leaderboard_handlers() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let ctx = context(OWNER, T0);
        let mut shop = InteractionRecorder::default();
        app.shop(&mut shop, &ctx);
        let mut rename = InteractionRecorder::default();
        app.rename(&mut rename, &ctx, "Sir Blep");
        let mut graveyard = InteractionRecorder::default();
        app.graveyard(&mut graveyard, &ctx, OWNER, "Owner");
        let mut leaderboard = InteractionRecorder::default();
        app.leaderboard(&mut leaderboard, &ctx);

        for interaction in [&shop, &rename, &graveyard, &leaderboard] {
            assert_eq!(
                interaction
                    .events
                    .iter()
                    .filter(|event| matches!(event, InteractionEvent::Followup(_)))
                    .count(),
                1
            );
        }
        assert_eq!(
            app.service()
                .active_pet(OWNER, GUILD)
                .map(|pet| pet.name.as_str()),
            Some("Sir Blep")
        );
    }

    #[test]
    fn test_unhatched_species_discount_is_hidden_from_shop_and_buttons() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Secret", "standard");
        let mut shop = InteractionRecorder::default();
        app.shop(&mut shop, &context(OWNER, T0));
        let shop_embed = payload_embed(&shop);
        assert!(
            shop_embed
                .fields
                .iter()
                .all(|field| !field.name.contains("discounted!"))
        );
        assert!(
            shop_embed
                .fields
                .iter()
                .any(|field| field.name.contains("Cheese — 30"))
        );
        let mut status = InteractionRecorder::default();
        app.status(&mut status, &context(OWNER, T0), OWNER, "Owner", false);
        let labels = status
            .last_payload()
            .and_then(|payload| payload.view.as_ref())
            .expect("status view")
            .buttons
            .iter()
            .map(|button| button.label.as_str())
            .collect::<BTreeSet<_>>();
        assert!(labels.contains("Buy Roshan's Cheese (30)"));
    }

    #[test]
    fn test_living_status_embed_shows_passive_mining_progress() {
        let pet = make_pet();
        let status = PetStatus {
            pet: Some(pet.clone()),
            hunger: 55,
            stage: Some(PetStage::Baby),
            mood: Some(PetMood::Content),
            age_seconds: DAY,
            supplies: None,
            last_dead: None,
            dig_work_units: 12 * DAY + DAY / 2,
            dig_work_rate: 8,
            evolution_hint: None,
        };

        let embed = build_status_embed(&status, 20, pet.hatched_at + DAY, "Owner", 20);

        assert_eq!(embed.field_value("Fullness"), Some("`██████░░░░` 55/100"));
        assert_eq!(
            embed.field_value("Mining"),
            Some("12.5/36 blocks banked · +8/day")
        );
    }

    #[test]
    fn test_food_ui_describes_positive_fullness() {
        let choices = FOOD_ITEMS
            .iter()
            .map(|food| {
                format!(
                    "{} ({} JC, +{} fullness)",
                    food.display_name, food.cost, food.restore
                )
            })
            .collect::<Vec<_>>();
        assert!(
            choices
                .iter()
                .all(|choice| choice.contains("fullness") && !choice.contains("hunger"))
        );
        let shop = build_shop_embed(None, "common_cama", Some(100));
        assert!(
            shop.fields[..3].iter().all(|field| {
                field.value.contains("fullness") && !field.value.contains("hunger")
            })
        );
    }

    #[test]
    fn test_owner_check_blocks_other_users() {
        let view = StatusView::new(OWNER, Some(GUILD), None, "common_cama", true);
        let blocked = view
            .interaction_check(555)
            .expect_err("non-owner must be blocked");
        assert!(
            blocked
                .content
                .as_deref()
                .expect("owner error")
                .to_lowercase()
                .contains("not your cama")
        );
        assert!(view.interaction_check(OWNER).is_ok());
    }

    #[test]
    fn test_feed_button_disabled_without_stock() {
        let supplies = BTreeMap::from([("tango".to_owned(), 0)]);
        let view = StatusView::new(OWNER, Some(GUILD), Some(&supplies), "common_cama", true);
        let feed_buttons = view
            .buttons
            .iter()
            .filter(|button| matches!(button.action, StatusAction::Feed(_)))
            .collect::<Vec<_>>();
        assert!(!feed_buttons.is_empty());
        assert!(feed_buttons.iter().all(|button| button.disabled));
    }

    #[test]
    fn test_buy_button_purchases_and_refreshes_panel() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let view = StatusView::new(OWNER, Some(GUILD), None, "", false);
        let action = view
            .buttons
            .iter()
            .find(|button| button.label.starts_with("Buy Tango"))
            .expect("buy Tango button")
            .action
            .clone();
        let mut interaction = InteractionRecorder::default();

        app.activate_status_button(&mut interaction, &context(OWNER, T0), &view, &action);

        assert!(matches!(
            interaction.events.first(),
            Some(InteractionEvent::Deferred(_))
        ));
        assert!(
            interaction
                .events
                .iter()
                .any(|event| matches!(event, InteractionEvent::EditOriginal(_)))
        );
        assert_eq!(
            app.service().stock(OWNER, GUILD),
            BTreeMap::from([("tango".to_owned(), 1)])
        );
    }

    #[test]
    fn test_feed_button_failure_reports_ephemerally() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let supplies = BTreeMap::from([("tango".to_owned(), 1)]);
        let view = StatusView::new(OWNER, Some(GUILD), Some(&supplies), "", true);
        let action = view
            .buttons
            .iter()
            .find(|button| button.label.starts_with("Feed Tango"))
            .expect("feed Tango button")
            .action
            .clone();
        let mut interaction = InteractionRecorder::default();

        app.activate_status_button(&mut interaction, &context(OWNER, T0), &view, &action);

        assert!(matches!(
            interaction.events.first(),
            Some(InteractionEvent::Deferred(_))
        ));
        assert!(payload_content(&interaction).starts_with("❌"));
        assert!(
            !interaction
                .events
                .iter()
                .any(|event| matches!(event, InteractionEvent::EditOriginal(_)))
        );
    }

    #[test]
    fn test_feed_button_refresh_uses_feed_flavor() {
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let _purchase = app.service_mut().buy(OWNER, Some(GUILD), "tango", 1);
        let feeding_time = T0 + EGG_HATCH_SECONDS + DAY;
        app.service_mut().now = feeding_time;
        app.flavor.responses.insert(
            PetFlavorEvent::Fed,
            "Button snack complete. Wool remains at maximum fluff.".to_owned(),
        );
        let supplies = BTreeMap::from([("tango".to_owned(), 1)]);
        let view = StatusView::new(OWNER, Some(GUILD), Some(&supplies), "common_cama", true);
        let action = StatusAction::Feed("tango".to_owned());
        let mut interaction = InteractionRecorder::default();

        app.activate_status_button(
            &mut interaction,
            &context(OWNER, feeding_time),
            &view,
            &action,
        );

        let embed = interaction
            .events
            .iter()
            .find_map(|event| match event {
                InteractionEvent::EditOriginal(payload) => payload.embed.as_ref(),
                _ => None,
            })
            .expect("refreshed status embed");
        assert_eq!(
            embed.field_value("💬 Cama chatter"),
            Some("Button snack complete. Wool remains at maximum fluff.")
        );
    }

    #[test]
    fn test_feed_button_defers_before_waiting_for_cold_flavor() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let mut app = make_app(FakeService::default());
        let _ = adopt_via_handler(&mut app, "Blep", "standard");
        let _purchase = app.service_mut().buy(OWNER, Some(GUILD), "tango", 1);
        let feeding_time = T0 + EGG_HATCH_SECONDS + DAY;
        app.service_mut().now = feeding_time;
        app.flavor.trace = Some(Rc::clone(&trace));
        app.flavor.responses.insert(
            PetFlavorEvent::Fed,
            "The snack committee has reached a fluffy consensus.".to_owned(),
        );
        let supplies = BTreeMap::from([("tango".to_owned(), 1)]);
        let view = StatusView::new(OWNER, Some(GUILD), Some(&supplies), "common_cama", true);
        let action = StatusAction::Feed("tango".to_owned());
        let mut interaction = TraceInteraction {
            trace: Rc::clone(&trace),
            events: Vec::new(),
        };

        app.activate_status_button(
            &mut interaction,
            &context(OWNER, feeding_time),
            &view,
            &action,
        );

        let events = trace.borrow();
        let defer_index = events
            .iter()
            .position(|event| *event == "defer")
            .expect("defer event");
        let flavor_index = events
            .iter()
            .position(|event| *event == "flavor")
            .expect("flavor event");
        assert!(defer_index < flavor_index);
        assert!(
            interaction
                .events
                .iter()
                .any(|event| matches!(event, InteractionEvent::EditOriginal(_)))
        );
    }

    #[test]
    fn altar_embeds_preserve_exact_preview_cancel_and_success_copy() {
        let mut service = FakeService::default();
        let adopted = service
            .adopt(OWNER, Some(GUILD), "Offering", "standard")
            .unwrap()
            .pet;
        service.now = adopted.hatched_at;
        let preview = service.sacrifice_preview(OWNER, Some(GUILD)).unwrap();

        let offer = build_altar_preview_embed(&preview);
        assert_eq!(offer.title, "🩸 The altar hungers for Offering");
        assert!(
            offer
                .description
                .contains("**Common 55% · Uncommon 33% · Rare 11% · Legendary 1%**")
        );
        assert!(
            offer
                .description
                .contains(&format!("Fee: **50** {JOPACOIN_EMOTE}"))
        );
        assert_eq!(offer.color, EmbedColor::Slate);

        let cancelled = build_altar_cancel_embed(&preview);
        assert_eq!(cancelled.title, "🕯️ The altar goes cold");
        assert_eq!(
            cancelled.description,
            "**Offering** lives to graze another day."
        );

        let outcome = service.sacrifice(OWNER, Some(GUILD), "Rebirth").unwrap();
        let success = build_altar_success_embed(&outcome);
        assert_eq!(success.title, "🩸 Offering was given to the altar");
        assert!(
            success
                .description
                .contains("The altar accepts **Offering**")
        );
        assert!(success.description.contains("a new egg: **Rebirth**"));
        assert!(
            success
                .description
                .contains("its species already chosen, already hidden")
        );
        assert_eq!(success.color, EmbedColor::Gold);
        assert_eq!(
            success.image.as_deref(),
            Some("attachment://pet-altar-1.png")
        );
    }

    #[test]
    fn test_notification_repo_supports_pet_type() {
        let mut preferences = NotificationPreferences::default();
        assert!(!preferences.pet_enabled(111, GUILD));
        preferences.set_pet_enabled(111, GUILD, true);
        assert!(preferences.pet_enabled(111, GUILD));
        assert_eq!(preferences.enabled_pet_users(GUILD), vec![111]);
    }

    #[test]
    fn test_schedule_pet_reminder_is_pref_gated() {
        let mut coordinator = ReminderCoordinator::new(T0);
        coordinator.schedule_pet_reminder(111, GUILD, T0 + 1_000, "Blep", false);
        assert!(!coordinator.has_pet_task(111, GUILD));
        coordinator.schedule_pet_reminder(111, GUILD, T0 + 1_000, "Blep", true);
        assert!(coordinator.has_pet_task(111, GUILD));
    }

    #[test]
    fn test_past_crossing_schedules_no_task() {
        let mut coordinator = ReminderCoordinator::new(T0);
        coordinator.schedule_pet_reminder(111, GUILD, 12_345, "Blep", true);
        assert!(!coordinator.has_pet_task(111, GUILD));
    }

    #[test]
    fn test_reschedule_all_recovers_pet_warning_after_restart() {
        let mut preferences = NotificationPreferences::default();
        preferences.set_pet_enabled(OWNER, GUILD, true);
        let mut source = FakeWarningSource::default();
        source.warnings.push_back(None);
        source
            .warnings
            .push_back(Some((T0 + 10_000, "Blep".to_owned())));
        let mut coordinator = ReminderCoordinator::new(T0);
        coordinator.reschedule_pet_warnings(&preferences, &[GUILD], Some(&mut source));
        assert!(!coordinator.has_pet_task(OWNER, GUILD));
        coordinator.reschedule_pet_warnings(&preferences, &[GUILD], Some(&mut source));
        assert!(coordinator.has_pet_task(OWNER, GUILD));
    }

    #[test]
    fn test_reschedule_all_skips_pets_when_service_disabled() {
        let mut preferences = NotificationPreferences::default();
        preferences.set_pet_enabled(OWNER, GUILD, true);
        let mut coordinator = ReminderCoordinator::new(T0);
        coordinator.reschedule_pet_warnings(&preferences, &[GUILD], None::<&mut FakeWarningSource>);
        assert!(!coordinator.has_pet_task(OWNER, GUILD));
    }

    #[test]
    fn test_rearm_warning_schedules_at_crossing() {
        let pet = owner_pet();
        let mut app = make_app(FakeService::default());
        app.reminders = Some(FakeReminder {
            enabled: true,
            ..FakeReminder::default()
        });
        app.rearm_warning(&pet, T0);
        let schedules = &app.reminders().expect("reminders").schedules;
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].2, pet.last_fed_at + 3 * DAY + DAY / 2);
        assert_eq!(schedules[0].3, "Blep");
        assert!(schedules[0].4);
    }

    #[test]
    fn test_returns_none_when_unconfigured() {
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);
        let channel = configured_pet_channel(None, GUILD, &mut discord);
        assert_eq!(channel, None);
        assert!(discord.resolution_requests.is_empty());
    }

    #[test]
    fn test_returns_channel_when_configured() {
        let mut discord = FakeDiscord::default().with_channel(GUILD, 42);
        let channel = configured_pet_channel(Some(42), GUILD, &mut discord);
        assert_eq!(channel, Some(42));
        assert_eq!(discord.resolution_requests, vec![(GUILD, 42)]);
    }
}
