//! Transport-independent policy for Discord boss encounter and duel views.
//!
//! Python remains the live Discord adapter during the side-by-side migration.
//! This module owns the state machine behind its views: authorization, modal
//! ownership, one-shot fight and resume guards, typed component identifiers,
//! callback ordering, presentation, and failure-tolerant delivery ports.

use std::fmt;

use crate::interaction_safety::{
    DeferPort, EditFailureScope, EditableMessage, SendPort, SendRequest, TransportError,
    edit_message_with_fallback, safe_defer,
};

pub const BOSS_MODAL_TIMEOUT_SECONDS: u64 = 600;
pub const BOSS_DUEL_TIMEOUT_SECONDS: u64 = 120;
pub const BOSS_WAGER_MAX_JC: i64 = 1_000;
pub const BOSS_WAGER_LABEL: &str = "Wager Amount (max 1,000 JC)";
pub const BOSS_WAGER_PLACEHOLDER: &str = "0-1000";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiskTier {
    Cautious,
    Bold,
    Reckless,
}

impl RiskTier {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cautious" => Some(Self::Cautious),
            "bold" => Some(Self::Bold),
            "reckless" => Some(Self::Reckless),
            _ => None,
        }
    }
}

impl fmt::Display for RiskTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cautious => "cautious",
            Self::Bold => "bold",
            Self::Reckless => "reckless",
        })
    }
}

/// The only component identifier accepted by a paused boss duel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DuelOptionId {
    pub option_index: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuelOptionIdError {
    WrongPrefix,
    MissingIndex,
    InvalidIndex,
}

impl DuelOptionId {
    pub fn parse(custom_id: &str) -> Result<Self, DuelOptionIdError> {
        let Some(index) = custom_id.strip_prefix("duel_opt_") else {
            return Err(DuelOptionIdError::WrongPrefix);
        };
        if index.is_empty() {
            return Err(DuelOptionIdError::MissingIndex);
        }
        if !index.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DuelOptionIdError::InvalidIndex);
        }
        index
            .parse::<u16>()
            .map(|option_index| Self { option_index })
            .map_err(|_| DuelOptionIdError::InvalidIndex)
    }
}

impl fmt::Display for DuelOptionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "duel_opt_{}", self.option_index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CarriedWager {
    pub risk_tier: RiskTier,
    pub amount: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossInfo {
    pub boss_id: String,
    pub name: String,
    pub dialogue: String,
    pub is_pinnacle: bool,
    pub phase: u8,
    pub wager_allowed: bool,
    pub encounter_key: String,
    pub carried_wager: Option<i64>,
}

impl Default for BossInfo {
    fn default() -> Self {
        Self {
            boss_id: "test_boss".to_owned(),
            name: "Test Boss".to_owned(),
            dialogue: "The tunnel shakes.".to_owned(),
            is_pinnacle: false,
            phase: 1,
            wager_allowed: true,
            encounter_key: "encounter-1".to_owned(),
            carried_wager: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PetAssist {
    pub pet_name: String,
    pub species_id: String,
    pub species_name: String,
    pub bonus_pct: i32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RoundEntry {
    pub pet_assist: Option<PetAssist>,
    pub pet_assist_damage: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuelOption {
    pub option_index: u16,
    pub label: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PendingPrompt {
    pub title: String,
    pub description: String,
    pub safe_option_index: u16,
    pub options: Vec<DuelOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BossResult {
    pub success: bool,
    pub error: Option<String>,
    pub won: bool,
    pub boss_name: String,
    pub payout: i64,
    pub jc_delta: i64,
    pub knockback: i32,
    pub win_chance_basis_points: u16,
    pub new_depth: i32,
    pub wager: i64,
    pub pending_prompt: Option<PendingPrompt>,
    pub phase2_incoming: bool,
    pub phase3_incoming: bool,
    pub is_pinnacle: bool,
    pub next_phase_title: Option<String>,
    pub phase2_name: Option<String>,
    pub phase2_title: Option<String>,
    pub dialogue: Option<String>,
    pub phase_event_flavor: Option<String>,
    pub phase_event_description: Option<String>,
    pub boss_id: Option<String>,
    pub boundary: Option<i32>,
    pub gear_broken: Vec<String>,
    pub pet_assist: Option<PetAssist>,
    pub round_log: Vec<RoundEntry>,
}

impl Default for BossResult {
    fn default() -> Self {
        Self {
            success: true,
            error: None,
            won: true,
            boss_name: "Test Boss".to_owned(),
            payout: 25,
            jc_delta: 25,
            knockback: 0,
            win_chance_basis_points: 5_000,
            new_depth: 100,
            wager: 0,
            pending_prompt: None,
            phase2_incoming: false,
            phase3_incoming: false,
            is_pinnacle: false,
            next_phase_title: None,
            phase2_name: None,
            phase2_title: None,
            dialogue: None,
            phase_event_flavor: None,
            phase_event_description: None,
            boss_id: None,
            boundary: None,
            gear_broken: Vec::new(),
            pet_assist: None,
            round_log: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceError(pub String);

pub trait BossFightPort {
    fn carried_wager(
        &mut self,
        user_id: i64,
        guild_id: Option<i64>,
    ) -> Result<Option<CarriedWager>, ServiceError>;

    fn start_boss_duel(
        &mut self,
        user_id: i64,
        guild_id: Option<i64>,
        risk_tier: RiskTier,
        wager: i64,
    ) -> Result<BossResult, ServiceError>;

    /// The service is still the authoritative stale-state gate. A real
    /// adapter checks the persisted active duel before applying this choice.
    fn resume_boss_duel(
        &mut self,
        user_id: i64,
        guild_id: Option<i64>,
        option_index: u16,
    ) -> Result<BossResult, ServiceError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalKind {
    Wager,
    RiskOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponseEvent {
    Initial { content: String, ephemeral: bool },
    Modal(ModalKind),
    Deferred { ephemeral: bool, thinking: bool },
    Followup { content: String, ephemeral: bool },
}

pub trait BossResponsePort: DeferPort {
    fn send_initial(&mut self, content: &str, ephemeral: bool);
    fn send_modal(&mut self, modal: ModalKind) -> Result<(), TransportError>;
    fn send_followup(&mut self, content: &str, ephemeral: bool);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewState {
    Ready,
    ModalOpen,
    Resolving,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FightEntry {
    Unauthorized,
    Duplicate,
    WagerModal,
    RiskModal,
    CarriedResolution,
    TransportRejected,
    ServiceFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonFightAction {
    Retreat,
    Scout,
    Cheer,
}

pub struct BossEncounterView {
    pub user_id: i64,
    pub guild_id: Option<i64>,
    pub boss_info: BossInfo,
    pub has_lantern: bool,
    pub callback_attached: bool,
    state: ViewState,
}

impl BossEncounterView {
    #[must_use]
    pub const fn state(&self) -> ViewState {
        self.state
    }

    #[must_use]
    pub fn new(
        user_id: i64,
        guild_id: Option<i64>,
        boss_info: BossInfo,
        has_lantern: bool,
        callback_attached: bool,
    ) -> Self {
        Self {
            user_id,
            guild_id,
            boss_info,
            has_lantern,
            callback_attached,
            state: ViewState::Ready,
        }
    }

    pub fn fight<S: BossFightPort, T: BossResponsePort + ResolutionPort>(
        &mut self,
        actor_id: i64,
        service: &mut S,
        transport: &mut T,
    ) -> FightEntry {
        if actor_id != self.user_id {
            transport.send_initial("Only the tunnel owner can fight.", true);
            return FightEntry::Unauthorized;
        }
        if self.state != ViewState::Ready {
            let _ = safe_defer(transport, false, false);
            return FightEntry::Duplicate;
        }
        self.state = ViewState::Resolving;

        if !self.boss_info.wager_allowed {
            return self.open_modal(transport, ModalKind::RiskOnly, FightEntry::RiskModal);
        }

        let carried = match service.carried_wager(self.user_id, self.guild_id) {
            Ok(carried) => carried,
            Err(error) => {
                self.state = ViewState::Ready;
                transport.send_followup(&error.0, true);
                return FightEntry::ServiceFailed;
            }
        };
        let Some(carried) = carried else {
            return self.open_modal(transport, ModalKind::Wager, FightEntry::WagerModal);
        };

        let _ = safe_defer(transport, false, false);
        match service.start_boss_duel(
            self.user_id,
            self.guild_id,
            carried.risk_tier,
            carried.amount,
        ) {
            Ok(result) => {
                route_resolution(
                    ResolutionOrigin::Carried,
                    result,
                    self.user_id,
                    self.guild_id,
                    self.callback_attached,
                    transport,
                );
                self.state = ViewState::Stopped;
                FightEntry::CarriedResolution
            }
            Err(error) => {
                transport.send_followup("Boss fight failed. Try again.", true);
                transport.warn(format!("Boss carried fight failed: {}", error.0));
                self.state = ViewState::Stopped;
                FightEntry::ServiceFailed
            }
        }
    }

    fn open_modal<T: BossResponsePort>(
        &mut self,
        transport: &mut T,
        modal: ModalKind,
        success: FightEntry,
    ) -> FightEntry {
        match transport.send_modal(modal) {
            Ok(()) => {
                self.state = ViewState::ModalOpen;
                success
            }
            Err(_) => {
                self.state = ViewState::Ready;
                FightEntry::TransportRejected
            }
        }
    }

    pub fn release_modal(&mut self) {
        if self.state == ViewState::ModalOpen {
            self.state = ViewState::Ready;
        }
    }

    pub fn finish_modal(&mut self) {
        if self.state == ViewState::ModalOpen {
            self.state = ViewState::Stopped;
        }
    }

    pub fn nonfight<T: BossResponsePort>(
        &mut self,
        actor_id: i64,
        action: NonFightAction,
        transport: &mut T,
    ) -> bool {
        let owner_only = matches!(action, NonFightAction::Retreat | NonFightAction::Scout);
        if owner_only && actor_id != self.user_id {
            transport.send_initial("Only the tunnel owner can do that.", true);
            return false;
        }
        let _ = safe_defer(transport, false, false);
        if action == NonFightAction::Retreat {
            self.state = ViewState::Stopped;
        }
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModalValidationError {
    InvalidRisk,
    InvalidWager,
    NegativeWager,
}

impl ModalValidationError {
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::InvalidRisk => "Invalid risk tier. Choose: cautious, bold, or reckless.",
            Self::InvalidWager => "Invalid wager amount. Please enter a number.",
            Self::NegativeWager => "Wager must be non-negative.",
        }
    }
}

pub struct BossWagerModal {
    pub callback_attached: bool,
}

impl BossWagerModal {
    pub fn submit<S: BossFightPort, T: BossResponsePort + ResolutionPort>(
        &self,
        parent: &mut BossEncounterView,
        risk_value: &str,
        wager_value: &str,
        service: &mut S,
        transport: &mut T,
    ) -> Result<(), ModalValidationError> {
        let tier = match RiskTier::parse(risk_value) {
            Some(tier) => tier,
            None => return Self::reject(parent, transport, ModalValidationError::InvalidRisk),
        };
        let amount = match wager_value.parse::<i64>() {
            Ok(amount) => amount,
            Err(_) => return Self::reject(parent, transport, ModalValidationError::InvalidWager),
        };
        if amount < 0 {
            return Self::reject(parent, transport, ModalValidationError::NegativeWager);
        }
        parent.finish_modal();
        let _ = safe_defer(transport, false, true);
        match service.start_boss_duel(parent.user_id, parent.guild_id, tier, amount) {
            Ok(result) => route_resolution(
                ResolutionOrigin::Modal,
                result,
                parent.user_id,
                parent.guild_id,
                self.callback_attached,
                transport,
            ),
            Err(error) => {
                transport.warn(format!("Boss wager fight failed: {}", error.0));
                transport.send_followup("Boss fight failed. Try again.", true);
            }
        }
        Ok(())
    }

    fn reject<T: BossResponsePort>(
        parent: &mut BossEncounterView,
        transport: &mut T,
        error: ModalValidationError,
    ) -> Result<(), ModalValidationError> {
        parent.release_modal();
        transport.send_initial(error.message(), true);
        Err(error)
    }

    pub fn on_timeout(parent: &mut BossEncounterView) {
        parent.release_modal();
    }
}

pub struct BossRiskModal {
    pub callback_attached: bool,
}

impl BossRiskModal {
    pub fn submit<S: BossFightPort, T: BossResponsePort + ResolutionPort>(
        &self,
        parent: &mut BossEncounterView,
        risk_value: &str,
        service: &mut S,
        transport: &mut T,
    ) -> Result<(), ModalValidationError> {
        let Some(tier) = RiskTier::parse(risk_value) else {
            parent.release_modal();
            transport.send_initial(ModalValidationError::InvalidRisk.message(), true);
            return Err(ModalValidationError::InvalidRisk);
        };
        parent.finish_modal();
        let _ = safe_defer(transport, false, true);
        match service.start_boss_duel(parent.user_id, parent.guild_id, tier, 0) {
            Ok(result) => route_resolution(
                ResolutionOrigin::RiskModal,
                result,
                parent.user_id,
                parent.guild_id,
                self.callback_attached,
                transport,
            ),
            Err(error) => {
                transport.warn(format!("Boss phase risk fight failed: {}", error.0));
                transport.send_followup("Boss fight failed. Try again.", true);
            }
        }
        Ok(())
    }

    pub fn on_timeout(parent: &mut BossEncounterView) {
        parent.release_modal();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionOrigin {
    Carried,
    Modal,
    RiskModal,
    Resume,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderPlan {
    Error(String),
    Prompt {
        prompt: PendingPrompt,
        callback_attached: bool,
        replacement: bool,
    },
    PhaseCleared {
        embed: EmbedModel,
        callback_attached: bool,
    },
    Final(EmbedModel),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleEvent {
    Callback { user_id: i64, guild_id: Option<i64> },
    Render(RenderPlan),
    Warning(String),
}

pub trait ResolutionPort {
    fn notify_resolved(&mut self, user_id: i64, guild_id: Option<i64>) -> Result<(), String>;
    fn render(&mut self, plan: RenderPlan);
    fn warn(&mut self, message: String);
}

/// Convert a resolution into durable message content and apply the shared
/// edit/fallback policy. A failed or expired interaction edit is retried in
/// the channel without copying an interactive view onto a new message.
pub fn edit_render_with_fallback(
    message: Option<&mut dyn EditableMessage>,
    channel: Option<&mut dyn SendPort>,
    plan: &RenderPlan,
) -> Result<bool, TransportError> {
    let mut request = match plan {
        RenderPlan::Error(content) => SendRequest {
            content: Some(content.clone()),
            ..SendRequest::default()
        },
        RenderPlan::Prompt { prompt, .. } => SendRequest {
            embed: Some(format!("{}\n{}", prompt.title, prompt.description)),
            view: true,
            ..SendRequest::default()
        },
        RenderPlan::PhaseCleared { embed, .. } | RenderPlan::Final(embed) => SendRequest {
            embed: Some(format!("{}\n{}", embed.title, embed.description)),
            ..SendRequest::default()
        },
    };
    edit_message_with_fallback(message, channel, &mut request, EditFailureScope::Any)
}

pub fn route_resolution<T: ResolutionPort>(
    origin: ResolutionOrigin,
    result: BossResult,
    user_id: i64,
    guild_id: Option<i64>,
    callback_attached: bool,
    transport: &mut T,
) {
    if !result.success {
        transport.render(RenderPlan::Error(
            result
                .error
                .unwrap_or_else(|| "Boss fight failed.".to_owned()),
        ));
        return;
    }
    if let Some(prompt) = result.pending_prompt.clone() {
        transport.render(RenderPlan::Prompt {
            prompt,
            callback_attached,
            replacement: origin == ResolutionOrigin::Resume,
        });
        return;
    }

    if callback_attached && let Err(error) = transport.notify_resolved(user_id, guild_id) {
        transport.warn(format!(
            "Boss resolved callback failed for user {user_id} in guild {}: {error}",
            guild_id.map_or_else(|| "None".to_owned(), |id| id.to_string())
        ));
    }

    if result.phase2_incoming || result.phase3_incoming {
        transport.render(RenderPlan::PhaseCleared {
            embed: phase_cleared_embed(&result),
            callback_attached,
        });
    } else {
        transport.render(RenderPlan::Final(build_boss_result_embed(&result)));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubmitOutcome {
    Unauthorized,
    Malformed,
    Duplicate,
    Resolved,
    ServiceFailed,
}

pub struct BossDuelView {
    pub user_id: i64,
    pub guild_id: Option<i64>,
    pub safe_option_index: u16,
    pub options: Vec<DuelOption>,
    pub callback_attached: bool,
    resolved: bool,
}

impl BossDuelView {
    #[must_use]
    pub fn new(
        user_id: i64,
        guild_id: Option<i64>,
        prompt: &PendingPrompt,
        callback_attached: bool,
    ) -> Self {
        Self {
            user_id,
            guild_id,
            safe_option_index: prompt.safe_option_index,
            options: prompt.options.clone(),
            callback_attached,
            resolved: false,
        }
    }

    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        self.resolved
    }

    pub fn click<S: BossFightPort, T: BossResponsePort + ResolutionPort>(
        &mut self,
        actor_id: i64,
        custom_id: &str,
        service: &mut S,
        transport: &mut T,
    ) -> SubmitOutcome {
        if actor_id != self.user_id {
            transport.send_initial("Only the duelist can choose.", true);
            return SubmitOutcome::Unauthorized;
        }
        let Ok(option) = DuelOptionId::parse(custom_id) else {
            transport.send_initial("That boss choice is no longer valid.", true);
            return SubmitOutcome::Malformed;
        };
        if !self
            .options
            .iter()
            .any(|candidate| candidate.option_index == option.option_index)
        {
            transport.send_initial("That boss choice is no longer valid.", true);
            return SubmitOutcome::Malformed;
        }
        let _ = safe_defer(transport, false, false);
        self.submit(option.option_index, service, transport)
    }

    pub fn on_timeout<S: BossFightPort, T: ResolutionPort>(
        &mut self,
        service: &mut S,
        transport: &mut T,
    ) -> SubmitOutcome {
        self.submit(self.safe_option_index, service, transport)
    }

    pub fn submit<S: BossFightPort, T: ResolutionPort>(
        &mut self,
        option_index: u16,
        service: &mut S,
        transport: &mut T,
    ) -> SubmitOutcome {
        if self.resolved {
            return SubmitOutcome::Duplicate;
        }
        self.resolved = true;
        match service.resume_boss_duel(self.user_id, self.guild_id, option_index) {
            Ok(result) => {
                route_resolution(
                    ResolutionOrigin::Resume,
                    result,
                    self.user_id,
                    self.guild_id,
                    self.callback_attached,
                    transport,
                );
                SubmitOutcome::Resolved
            }
            Err(error) => {
                transport.warn(format!("Boss duel resume failed: {}", error.0));
                transport.render(RenderPlan::Error("Duel resume failed.".to_owned()));
                SubmitOutcome::ServiceFailed
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmbedModel {
    pub title: String,
    pub description: String,
    pub fields: Vec<EmbedField>,
    pub image: Option<String>,
}

impl EmbedModel {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
    }

    fn add_field(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
        });
    }
}

fn pet_flavor(species_id: &str) -> &'static str {
    match species_id {
        "common_cama" => "barrels into the fray",
        "dromedary_cross" => "leans in with a stubborn shoulder-check",
        "banana_ears" => "hums a battle note and charges",
        "embergear_cama" => "builds steam and rockets forward",
        "jopacama" => "finds a suspiciously profitable weak spot",
        "pudge_cama" => "takes a bite out of the opposition",
        "courier_cama" => "delivers a hit straight to the boss",
        "riverglow_cama" => "skates through the dark and clips the boss",
        "aegis_cama" => "slams its glowing shell into the boss",
        "invoker_cama" => "assembles several extremely learned sparks",
        "crystal_cama" => "freezes the boss's footing",
        "prismwool_cama" => "refracts the torchlight into a sharp flash",
        "rama" => "spits with historic contempt",
        "moondrift_cama" => "drifts through the boss's guard",
        "sunspun_cama" => "flares gold and strikes",
        _ => "lunges at the boss",
    }
}

#[must_use]
pub fn format_pet_assist(assist: &PetAssist, bonus_damage: Option<i32>) -> String {
    let mut line = format!(
        "**{}** ({}) {}, lending a **{}%** damage assist.",
        assist.pet_name,
        assist.species_name,
        pet_flavor(&assist.species_id),
        assist.bonus_pct
    );
    if let Some(damage) = bonus_damage {
        if damage > 0 {
            line.push_str(&format!("\nIt contributed **{damage} bonus damage**."));
        } else {
            line.push_str("\nNo opening appeared this time.");
        }
    }
    line
}

fn result_pet_assist(result: &BossResult) -> Option<(PetAssist, i32)> {
    let mut assist = result.pet_assist.clone();
    let mut damage = 0;
    for round in &result.round_log {
        if assist.is_none() {
            assist.clone_from(&round.pet_assist);
        }
        damage += round.pet_assist_damage;
    }
    assist.map(|assist| (assist, damage))
}

fn add_gear_broken(embed: &mut EmbedModel, result: &BossResult) {
    if result.gear_broken.is_empty() {
        return;
    }
    let mut value = result
        .gear_broken
        .iter()
        .map(|name| format!("• **{name}**"))
        .collect::<Vec<_>>()
        .join("\n");
    value.push_str(
        "\nThese items stay equipped with their effects disabled until repaired. Use **Repair All** in `/dig gear`.",
    );
    embed.add_field("Gear Broken", value);
}

fn add_pet_assist(embed: &mut EmbedModel, result: &BossResult, include_outcome: bool) {
    if let Some((assist, damage)) = result_pet_assist(result) {
        embed.add_field(
            "Pet Assist",
            format_pet_assist(&assist, include_outcome.then_some(damage)),
        );
    }
}

#[must_use]
pub fn build_duel_prompt_embed(result: &BossResult) -> EmbedModel {
    let prompt = result.pending_prompt.as_ref();
    let mut embed = EmbedModel {
        title: prompt.map_or_else(|| "Choose".to_owned(), |value| value.title.clone()),
        description: prompt.map_or_else(String::new, |value| value.description.clone()),
        ..EmbedModel::default()
    };
    add_gear_broken(&mut embed, result);
    add_pet_assist(&mut embed, result, false);
    embed
}

#[must_use]
pub fn build_boss_result_embed(result: &BossResult) -> EmbedModel {
    let description = if result.won {
        format!(
            "Victory! You defeated **{}** and won **{:+}** JC profit!",
            result.boss_name, result.payout
        )
    } else {
        let loss = result
            .jc_delta
            .unsigned_abs()
            .max(result.wager.unsigned_abs());
        format!(
            "Defeat! **{}** overpowered you. You lost **{}** JC and were knocked back {} blocks.",
            result.boss_name, loss, result.knockback
        )
    };
    let mut embed = EmbedModel {
        title: "Boss Fight Result".to_owned(),
        description,
        ..EmbedModel::default()
    };
    add_gear_broken(&mut embed, result);
    add_pet_assist(&mut embed, result, true);
    embed.add_field(
        "Details",
        format!(
            "Pre-fight win chance: {}%",
            result.win_chance_basis_points / 100
        ),
    );
    embed
}

fn phase_cleared_embed(result: &BossResult) -> EmbedModel {
    EmbedModel {
        title: "Phase Cleared".to_owned(),
        description: format!("You broke **{}** — it staggers...", result.boss_name),
        ..EmbedModel::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretPhaseDefinition {
    pub title: String,
    pub dialogue: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EncounterPresentation {
    pub name: String,
    pub dialogue: String,
    pub secret: bool,
}

fn stable_unit_interval(seed: &str) -> f64 {
    // FNV-1a is deliberately specified here instead of relying on a process-
    // randomized hasher. The same encounter key therefore never re-rolls.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Avalanche nearby encounter timestamps; raw FNV values share high bits
    // for long common prefixes and would otherwise cluster at a 50% cutoff.
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 31;
    (hash as f64) / (u64::MAX as f64)
}

#[must_use]
pub fn resolve_encounter_presentation(
    info: &BossInfo,
    phase: u8,
    seed_key: &str,
    secret_chance: f64,
    secret_definition: Option<&SecretPhaseDefinition>,
    forced_roll: Option<f64>,
) -> EncounterPresentation {
    let normal = || EncounterPresentation {
        name: info.name.clone(),
        dialogue: info.dialogue.clone(),
        secret: false,
    };
    if phase != 3 || !info.is_pinnacle || info.boss_id.is_empty() {
        return normal();
    }
    let Some(secret) = secret_definition else {
        return normal();
    };
    if forced_roll.unwrap_or_else(|| stable_unit_interval(seed_key)) >= secret_chance {
        return normal();
    }
    let dialogue = if secret.dialogue.is_empty() {
        info.dialogue.clone()
    } else {
        let choice_seed = format!("{seed_key}:dialogue");
        let index = (stable_unit_interval(&choice_seed) * secret.dialogue.len() as f64) as usize;
        secret.dialogue[index.min(secret.dialogue.len() - 1)].clone()
    };
    EncounterPresentation {
        name: secret.title.clone(),
        dialogue,
        secret: true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtRequest {
    pub boss_id: String,
    pub phase: u8,
    pub layer_name: String,
    pub secret: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseTransitionPlan {
    pub transition: EmbedModel,
    pub encounter: EmbedModel,
    pub encounter_info: BossInfo,
    pub art: Option<ArtRequest>,
    pub callback_attached: bool,
}

#[must_use]
pub fn build_phase_transition(
    result: &BossResult,
    next_info: BossInfo,
    presentation: EncounterPresentation,
    layer_name: &str,
    callback_attached: bool,
) -> PhaseTransitionPlan {
    let next_title = result
        .next_phase_title
        .clone()
        .or_else(|| result.phase2_name.clone())
        .unwrap_or_else(|| "???".to_owned());
    let transition_title = result
        .phase2_title
        .clone()
        .or_else(|| result.next_phase_title.clone())
        .unwrap_or_else(|| next_title.clone());
    let dialogue = result.dialogue.as_deref().unwrap_or("...");
    let mut transition = EmbedModel {
        title: format!("{transition_title} Emerges!"),
        description: format!(
            "**{}** is transforming!\n\n**{next_title}**\n*{dialogue}*",
            result.boss_name
        ),
        ..EmbedModel::default()
    };
    if result.wager > 0 {
        transition.add_field(
            "\u{200b}",
            format!(
                "Your **{}** JC wager rides on the next phase.",
                result.wager
            ),
        );
    }
    add_gear_broken(&mut transition, result);
    add_pet_assist(&mut transition, result, true);
    if result.is_pinnacle {
        let event = [
            result
                .phase_event_flavor
                .as_deref()
                .map(|value| format!("*{value}*")),
            result.phase_event_description.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        if !event.is_empty() {
            transition.add_field("Phase shift", event);
        }
    }

    let mut encounter = EmbedModel {
        title: format!("Boss Encountered: {}!", presentation.name),
        description: presentation.dialogue,
        ..EmbedModel::default()
    };
    if let Some(amount) = next_info.carried_wager.filter(|amount| *amount > 0) {
        encounter.add_field(
            "Carried Wager",
            format!(
                "**{}** JC is already riding on this phase.",
                format_number(amount)
            ),
        );
    }
    let phase = if result.phase3_incoming { 3 } else { 2 };
    let art = next_info.is_pinnacle.then(|| ArtRequest {
        boss_id: next_info.boss_id.clone(),
        phase,
        layer_name: layer_name.to_owned(),
        secret: presentation.secret,
    });
    if let Some(art) = &art {
        encounter.image = Some(format!("attachment://pinnacle_phase_{}.png", art.phase));
    }
    PhaseTransitionPlan {
        transition,
        encounter,
        encounter_info: next_info,
        art,
        callback_attached,
    }
}

fn format_number(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(char::from(byte));
    }
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

#[cfg(test)]
#[path = "boss_encounter_view_guard/tests.rs"]
mod tests;
