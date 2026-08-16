//! Discord-facing orchestration and presentation models for pet brawls.
//!
//! The transport is represented by typed, synchronous in-memory ports so the
//! command contract can be verified without a Discord connection.

use std::collections::BTreeMap;

use cama_db::pet_brawl_repository::SweepResult;
use cama_domain::pet::{PET_BRAWL_TURN_SECONDS, Pet, SpeciesTier, get_species};
use cama_domain::pet_brawl::{
    BrawlRng, Duelist, MAX_ROUNDS, PetBrawl, PetBrawlMove, PetBrawlState, SHELL_BLURB, move_name,
    resolve_round, trait_blurb,
};
use cama_domain::service_result::ServiceResult;

use crate::pet_brawl::{
    AcceptOutcome, ChallengeOutcome, Clock, DecisiveOutcome, DrawOutcome, PetBrawlPort,
    PetBrawlService, PetEvolutionPort, PetPort, PortResult, ServiceRng, SettleOutcome,
    SoloTrainingOutcome, WagerInput,
};
use crate::python_random::PythonRandom;

pub const SAFE_MOVE: PetBrawlMove = PetBrawlMove::Spit;
pub const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";
pub const COLOR_BLUE: u32 = 0x0034_98db;
pub const COLOR_GREEN: u32 = 0x0057_f287;
pub const COLOR_ORANGE: u32 = 0x00f3_9c12;
pub const COLOR_DEAD: u32 = 0x005d_6d7e;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmbedModel {
    pub title: String,
    pub description: String,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
    pub image_attachment: Option<String>,
    pub color: Option<u32>,
}

impl EmbedModel {
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&EmbedField> {
        self.fields.iter().find(|field| field.name == name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Success,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ButtonModel {
    pub label: String,
    pub emoji: Option<String>,
    pub style: ButtonStyle,
    pub custom_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChallengeViewModel {
    pub brawl_id: i64,
    pub buttons: Vec<ButtonModel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BattleViewModel {
    pub brawl_id: i64,
    pub round_no: u8,
    pub buttons: Vec<ButtonModel>,
    pub custom_ids: Vec<String>,
}

impl BattleViewModel {
    #[must_use]
    pub fn new(brawl_id: i64, round_no: u8) -> Self {
        let buttons = PetBrawlMove::ALL
            .iter()
            .map(|move_| ButtonModel {
                label: move_name("", *move_).to_owned(),
                emoji: Some(move_.emoji().to_owned()),
                style: ButtonStyle::Primary,
                custom_id: Some(format!(
                    "pet_brawl:{brawl_id}:{round_no}:{}",
                    move_.as_str()
                )),
            })
            .collect::<Vec<_>>();
        Self {
            brawl_id,
            round_no,
            custom_ids: buttons
                .iter()
                .filter_map(|button| button.custom_id.clone())
                .collect(),
            buttons,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewModel {
    Challenge(ChallengeViewModel),
    Battle(BattleViewModel),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OutboundMessage {
    pub content: Option<String>,
    pub embed: Option<EmbedModel>,
    pub view: Option<ViewModel>,
    pub ephemeral: bool,
    pub attachments: Vec<String>,
    pub allowed_user_mentions: Vec<i64>,
}

impl OutboundMessage {
    #[must_use]
    pub fn text(content: impl Into<String>, ephemeral: bool) -> Self {
        Self {
            content: Some(content.into()),
            ephemeral,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscordEvent {
    Initial(OutboundMessage),
    Deferred {
        ephemeral: bool,
    },
    FollowupAttempt(OutboundMessage),
    Edited {
        message_id: Option<u64>,
        message: OutboundMessage,
    },
    WaitingForSessionLock {
        brawl_id: i64,
    },
}

/// Minimal Discord adapter used by the command layer.
///
/// A real adapter may await these operations. The in-memory implementation
/// deliberately records the defer before a simulated session-lock wait, which
/// preserves Discord's three-second acknowledgement invariant.
pub trait DiscordPort {
    fn initial_response(&mut self, message: OutboundMessage);
    fn defer(&mut self, ephemeral: bool) -> bool;
    fn followup(&mut self, message: OutboundMessage) -> Option<u64>;
    fn edit_or_send(&mut self, message_id: Option<u64>, message: OutboundMessage);
    fn waiting_for_session_lock(&mut self, brawl_id: i64);
}

#[derive(Clone, Debug)]
pub struct InMemoryDiscord {
    pub events: Vec<DiscordEvent>,
    pub allow_defer: bool,
    pub deliver_followups: bool,
    pub fail_next_followup: bool,
    next_message_id: u64,
}

impl Default for InMemoryDiscord {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            allow_defer: true,
            deliver_followups: true,
            fail_next_followup: false,
            next_message_id: 1,
        }
    }
}

impl InMemoryDiscord {
    #[must_use]
    pub fn last_message(&self) -> Option<&OutboundMessage> {
        self.events.iter().rev().find_map(|event| match event {
            DiscordEvent::Initial(message) | DiscordEvent::FollowupAttempt(message) => {
                Some(message)
            }
            DiscordEvent::Edited { message, .. } => Some(message),
            DiscordEvent::Deferred { .. } | DiscordEvent::WaitingForSessionLock { .. } => None,
        })
    }

    #[must_use]
    pub fn defer_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, DiscordEvent::Deferred { .. }))
            .count()
    }
}

impl DiscordPort for InMemoryDiscord {
    fn initial_response(&mut self, message: OutboundMessage) {
        self.events.push(DiscordEvent::Initial(message));
    }

    fn defer(&mut self, ephemeral: bool) -> bool {
        if self.allow_defer {
            self.events.push(DiscordEvent::Deferred { ephemeral });
            true
        } else {
            false
        }
    }

    fn followup(&mut self, message: OutboundMessage) -> Option<u64> {
        self.events.push(DiscordEvent::FollowupAttempt(message));
        if self.fail_next_followup {
            self.fail_next_followup = false;
            return None;
        }
        if !self.deliver_followups {
            return None;
        }
        let message_id = self.next_message_id;
        self.next_message_id += 1;
        Some(message_id)
    }

    fn edit_or_send(&mut self, message_id: Option<u64>, message: OutboundMessage) {
        self.events.push(DiscordEvent::Edited {
            message_id,
            message,
        });
    }

    fn waiting_for_session_lock(&mut self, brawl_id: i64) {
        self.events
            .push(DiscordEvent::WaitingForSessionLock { brawl_id });
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InteractionModel {
    pub user_id: i64,
    pub display_name: String,
    pub is_bot: bool,
    pub guild_id: Option<i64>,
    pub channel_id: i64,
    pub parent_channel_id: Option<i64>,
    pub rate_allowed: bool,
    pub retry_after_seconds: i64,
}

impl InteractionModel {
    #[must_use]
    pub fn in_channel(&self, channel_id: i64) -> bool {
        self.channel_id == channel_id || self.parent_channel_id == Some(channel_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetMember {
    pub id: i64,
    pub is_bot: bool,
}

/// The already-ported application service contract consumed by commands.
pub trait PetBrawlApplication {
    fn train_solo(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SoloTrainingOutcome>;
    fn challenge(
        &mut self,
        challenger_id: i64,
        recipient_id: i64,
        guild_id: Option<i64>,
        channel_id: i64,
        wager: WagerInput,
    ) -> ServiceResult<ChallengeOutcome>;
    fn accept(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
    ) -> ServiceResult<AcceptOutcome>;
    fn decline(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
    ) -> ServiceResult<()>;
    fn withdraw(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        challenger_id: i64,
    ) -> ServiceResult<()>;
    fn void(&mut self, brawl_id: i64, guild_id: Option<i64>) -> ServiceResult<()>;
    fn settle(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        final_state: &PetBrawlState,
        rounds: i64,
    ) -> ServiceResult<SettleOutcome>;
    fn sweep_stale(&mut self) -> PortResult<SweepResult>;
}

impl<P, B, C, R, E> PetBrawlApplication for PetBrawlService<P, B, C, R, E>
where
    P: PetPort,
    B: PetBrawlPort,
    C: Clock,
    R: ServiceRng,
    E: PetEvolutionPort,
{
    fn train_solo(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SoloTrainingOutcome> {
        PetBrawlService::train_solo(self, discord_id, guild_id)
    }

    fn challenge(
        &mut self,
        challenger_id: i64,
        recipient_id: i64,
        guild_id: Option<i64>,
        channel_id: i64,
        wager: WagerInput,
    ) -> ServiceResult<ChallengeOutcome> {
        PetBrawlService::challenge(
            self,
            challenger_id,
            recipient_id,
            guild_id,
            channel_id,
            wager,
        )
    }

    fn accept(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
    ) -> ServiceResult<AcceptOutcome> {
        PetBrawlService::accept(self, brawl_id, guild_id, recipient_id)
    }

    fn decline(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        recipient_id: i64,
    ) -> ServiceResult<()> {
        PetBrawlService::decline(self, brawl_id, guild_id, recipient_id)
    }

    fn withdraw(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        challenger_id: i64,
    ) -> ServiceResult<()> {
        PetBrawlService::withdraw(self, brawl_id, guild_id, challenger_id)
    }

    fn void(&mut self, brawl_id: i64, guild_id: Option<i64>) -> ServiceResult<()> {
        PetBrawlService::void(self, brawl_id, guild_id)
    }

    fn settle(
        &mut self,
        brawl_id: i64,
        guild_id: Option<i64>,
        final_state: &PetBrawlState,
        rounds: i64,
    ) -> ServiceResult<SettleOutcome> {
        PetBrawlService::settle(self, brawl_id, guild_id, final_state, rounds)
    }

    fn sweep_stale(&mut self) -> PortResult<SweepResult> {
        PetBrawlService::sweep_stale(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChallengeHandle {
    pub brawl: PetBrawl,
    pub message_id: Option<u64>,
    responded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BattleHandle {
    pub brawl_id: i64,
    pub message_id: Option<u64>,
    pub round_no: u8,
    resolved: bool,
}

impl BattleHandle {
    #[must_use]
    pub fn view_model(&self) -> BattleViewModel {
        BattleViewModel::new(self.brawl_id, self.round_no)
    }

    /// Return the process-local handle for the next round after a resolved
    /// round. The command layer marks the consumed handle resolved while it
    /// computes the round; the runtime must retain a fresh unresolved handle
    /// for the next interaction and timeout.
    #[must_use]
    pub fn next_round_handle(&self) -> Self {
        Self {
            brawl_id: self.brawl_id,
            message_id: self.message_id,
            round_no: self.round_no.saturating_add(1),
            resolved: false,
        }
    }
}

/// `random.randint` over the shared CPython MT19937 port. The brawl service
/// hands this layer the same unsigned 64-bit seed used by
/// `random.Random(seed)`, so a recorded seed replays the same round sequence
/// in both runtimes.
#[derive(Clone, Debug)]
struct PythonCombatRng {
    inner: PythonRandom,
}

impl PythonCombatRng {
    fn new(seed: u64) -> Self {
        Self {
            inner: PythonRandom::from_u64_seed(seed),
        }
    }
}

impl BrawlRng for PythonCombatRng {
    fn randint(&mut self, low: i32, high: i32) -> i32 {
        let span = usize::try_from(high - low + 1).expect("combat range is positive");
        low + i32::try_from(self.inner.randbelow(span)).expect("combat range fits i32")
    }
}

#[derive(Clone, Debug)]
struct BattleSession {
    guild_id: Option<i64>,
    state: PetBrawlState,
    rng: PythonCombatRng,
    picks: BTreeMap<&'static str, PetBrawlMove>,
    last_log: Vec<String>,
    message_id: Option<u64>,
    consecutive_full_timeouts: u8,
    lock_held: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickOutcome {
    Rejected,
    WaitingForLock,
    LockedIn,
    RoundResolved,
    BattleFinished,
}

pub struct PetBrawlCommands<S>
where
    S: PetBrawlApplication,
{
    service: S,
    pet_channel_id: i64,
    sessions: BTreeMap<i64, BattleSession>,
}

impl<S> PetBrawlCommands<S>
where
    S: PetBrawlApplication,
{
    #[must_use]
    pub const fn new(service: S, pet_channel_id: i64) -> Self {
        Self {
            service,
            pet_channel_id,
            sessions: BTreeMap::new(),
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
    pub fn has_session(&self, brawl_id: i64) -> bool {
        self.sessions.contains_key(&brawl_id)
    }

    #[must_use]
    pub fn session_state(&self, brawl_id: i64) -> Option<&PetBrawlState> {
        self.sessions.get(&brawl_id).map(|session| &session.state)
    }

    #[must_use]
    pub fn session_picks(&self, brawl_id: i64) -> Option<&BTreeMap<&'static str, PetBrawlMove>> {
        self.sessions.get(&brawl_id).map(|session| &session.picks)
    }

    #[must_use]
    pub fn session_last_log(&self, brawl_id: i64) -> Option<&[String]> {
        self.sessions
            .get(&brawl_id)
            .map(|session| session.last_log.as_slice())
    }

    #[must_use]
    pub fn session_full_timeouts(&self, brawl_id: i64) -> Option<u8> {
        self.sessions
            .get(&brawl_id)
            .map(|session| session.consecutive_full_timeouts)
    }

    pub fn set_session_state(&mut self, brawl_id: i64, state: PetBrawlState) {
        if let Some(session) = self.sessions.get_mut(&brawl_id) {
            session.state = state;
        }
    }

    pub fn set_session_lock_held(&mut self, brawl_id: i64, held: bool) {
        if let Some(session) = self.sessions.get_mut(&brawl_id) {
            session.lock_held = held;
        }
    }

    pub fn train<D: DiscordPort>(&mut self, interaction: &InteractionModel, discord: &mut D) {
        if !discord.defer(true) {
            return;
        }
        match self
            .service
            .train_solo(interaction.user_id, interaction.guild_id)
        {
            ServiceResult::Success(outcome) => {
                let sessions = outcome.sessions_used;
                let session_word = if sessions == 1 { "session" } else { "sessions" };
                let mut lines = vec![
                    outcome.flavor,
                    format!(
                        "{sessions} banked {session_word} → **+{} XP** ({}/20).",
                        outcome.xp_delta, outcome.career.xp
                    ),
                ];
                if let Some(stat_gain) = outcome.stat_gain {
                    lines.push(format!("✨ **{} +1**", stat_gain.to_uppercase()));
                }
                if let Some(build) = outcome.career.build {
                    lines.push(format!("Battle style: **{build}**"));
                }
                if outcome.sessions_available != 0 {
                    let plural = if outcome.sessions_available == 1 {
                        ""
                    } else {
                        "s"
                    };
                    lines.push(format!(
                        "{} banked session{plural} remain.",
                        outcome.sessions_available
                    ));
                }
                let _ignored = discord.followup(OutboundMessage::text(lines.join("\n"), true));
            }
            ServiceResult::Failure { error, .. } => {
                let _ignored = discord.followup(OutboundMessage::text(format!("❌ {error}"), true));
            }
        }
    }

    pub fn brawl<D: DiscordPort>(
        &mut self,
        interaction: &InteractionModel,
        target: TargetMember,
        wager: Option<i64>,
        discord: &mut D,
    ) -> Option<ChallengeHandle> {
        if !interaction.rate_allowed {
            discord.initial_response(OutboundMessage::text(
                format!("⏳ Please wait {}s.", interaction.retry_after_seconds),
                true,
            ));
            return None;
        }
        if target.is_bot {
            discord.initial_response(OutboundMessage::text("Bots keep no pets. Yet.", true));
            return None;
        }
        if !interaction.in_channel(self.pet_channel_id) {
            discord.initial_response(OutboundMessage::text(
                "Pet brawls belong in the pet arena channel.",
                true,
            ));
            return None;
        }
        if !discord.defer(false) {
            return None;
        }
        let challenge = match self.service.challenge(
            interaction.user_id,
            target.id,
            interaction.guild_id,
            interaction.channel_id,
            WagerInput::Integer(wager.unwrap_or(0)),
        ) {
            ServiceResult::Success(challenge) => challenge,
            ServiceResult::Failure { error, .. } => {
                let _ignored = discord.followup(OutboundMessage::text(format!("❌ {error}"), true));
                return None;
            }
        };
        let view = ChallengeViewModel {
            brawl_id: challenge.brawl.brawl_id,
            buttons: vec![
                ButtonModel {
                    label: "Accept".to_owned(),
                    emoji: Some("⚔️".to_owned()),
                    style: ButtonStyle::Success,
                    custom_id: None,
                },
                ButtonModel {
                    label: "Decline".to_owned(),
                    emoji: None,
                    style: ButtonStyle::Secondary,
                    custom_id: None,
                },
                ButtonModel {
                    label: "Withdraw".to_owned(),
                    emoji: None,
                    style: ButtonStyle::Secondary,
                    custom_id: None,
                },
            ],
        };
        let message = OutboundMessage {
            content: Some(format!("<@{}> — you've been challenged!", target.id)),
            embed: Some(build_challenge_embed(
                &challenge.brawl,
                &challenge.challenger_pet,
                &challenge.recipient_pet,
            )),
            view: Some(ViewModel::Challenge(view)),
            attachments: vec!["pet-brawl-versus.png".to_owned()],
            allowed_user_mentions: vec![target.id],
            ..OutboundMessage::default()
        };
        let message_id = discord.followup(message);
        if message_id.is_none() {
            let _ignored = self
                .service
                .void(challenge.brawl.brawl_id, interaction.guild_id);
            return None;
        }
        Some(ChallengeHandle {
            brawl: challenge.brawl,
            message_id,
            responded: false,
        })
    }

    pub fn accept<D: DiscordPort>(
        &mut self,
        challenge: &mut ChallengeHandle,
        interaction: &InteractionModel,
        discord: &mut D,
    ) -> Option<BattleHandle> {
        if interaction.user_id != challenge.brawl.recipient_id {
            discord.initial_response(OutboundMessage::text(
                "This challenge isn't yours to answer.",
                true,
            ));
            return None;
        }
        if challenge.responded {
            discord.initial_response(OutboundMessage::text("Already answered.", true));
            return None;
        }
        challenge.responded = true;
        if !discord.defer(false) {
            challenge.responded = false;
            return None;
        }
        let guild_id = Some(challenge.brawl.guild_id);
        match self
            .service
            .accept(challenge.brawl.brawl_id, guild_id, interaction.user_id)
        {
            ServiceResult::Success(outcome) => {
                let handle = BattleHandle {
                    brawl_id: challenge.brawl.brawl_id,
                    message_id: challenge.message_id,
                    round_no: outcome.state.round_no,
                    resolved: false,
                };
                self.sessions.insert(
                    challenge.brawl.brawl_id,
                    BattleSession {
                        guild_id,
                        state: outcome.state,
                        rng: PythonCombatRng::new(outcome.combat_seed),
                        picks: BTreeMap::new(),
                        last_log: Vec::new(),
                        message_id: challenge.message_id,
                        consecutive_full_timeouts: 0,
                        lock_held: false,
                    },
                );
                let state = &self
                    .sessions
                    .get(&challenge.brawl.brawl_id)
                    .expect("new session exists")
                    .state;
                discord.edit_or_send(
                    challenge.message_id,
                    OutboundMessage {
                        embed: Some(build_battle_embed(state, &[], &BTreeMap::new())),
                        view: Some(ViewModel::Battle(handle.view_model())),
                        ..OutboundMessage::default()
                    },
                );
                Some(handle)
            }
            ServiceResult::Failure { error, error_code } => {
                challenge.responded = false;
                let _ignored = discord.followup(OutboundMessage::text(format!("❌ {error}"), true));
                if error_code.as_deref() == Some("pet_dead") {
                    challenge.responded = true;
                    discord.edit_or_send(
                        challenge.message_id,
                        OutboundMessage {
                            embed: Some(build_void_embed(&error)),
                            ..OutboundMessage::default()
                        },
                    );
                }
                None
            }
        }
    }

    pub fn decline<D: DiscordPort>(
        &mut self,
        challenge: &mut ChallengeHandle,
        interaction: &InteractionModel,
        discord: &mut D,
    ) {
        if interaction.user_id != challenge.brawl.recipient_id {
            discord.initial_response(OutboundMessage::text(
                "This challenge isn't yours to answer.",
                true,
            ));
            return;
        }
        if challenge.responded {
            discord.initial_response(OutboundMessage::text("Already answered.", true));
            return;
        }
        challenge.responded = true;
        if !discord.defer(false) {
            challenge.responded = false;
            return;
        }
        match self.service.decline(
            challenge.brawl.brawl_id,
            Some(challenge.brawl.guild_id),
            interaction.user_id,
        ) {
            ServiceResult::Success(()) => discord.edit_or_send(
                challenge.message_id,
                OutboundMessage {
                    embed: Some(build_declined_embed(interaction.user_id)),
                    ..OutboundMessage::default()
                },
            ),
            ServiceResult::Failure { error, .. } => {
                challenge.responded = false;
                let _ignored = discord.followup(OutboundMessage::text(format!("❌ {error}"), true));
            }
        }
    }

    pub fn withdraw<D: DiscordPort>(
        &mut self,
        challenge: &mut ChallengeHandle,
        interaction: &InteractionModel,
        discord: &mut D,
    ) {
        if interaction.user_id != challenge.brawl.challenger_id {
            discord.initial_response(OutboundMessage::text(
                "Only the challenger can withdraw.",
                true,
            ));
            return;
        }
        if challenge.responded {
            discord.initial_response(OutboundMessage::text("Already answered.", true));
            return;
        }
        challenge.responded = true;
        if !discord.defer(false) {
            challenge.responded = false;
            return;
        }
        match self.service.withdraw(
            challenge.brawl.brawl_id,
            Some(challenge.brawl.guild_id),
            interaction.user_id,
        ) {
            ServiceResult::Success(()) => discord.edit_or_send(
                challenge.message_id,
                OutboundMessage {
                    embed: Some(build_void_embed("The challenger thought better of it.")),
                    ..OutboundMessage::default()
                },
            ),
            ServiceResult::Failure { error, .. } => {
                challenge.responded = false;
                let _ignored = discord.followup(OutboundMessage::text(format!("❌ {error}"), true));
            }
        }
    }

    pub fn challenge_timeout<D: DiscordPort>(
        &mut self,
        challenge: &mut ChallengeHandle,
        discord: &mut D,
    ) {
        if challenge.responded {
            return;
        }
        challenge.responded = true;
        let _ignored = self
            .service
            .void(challenge.brawl.brawl_id, Some(challenge.brawl.guild_id));
        discord.edit_or_send(
            challenge.message_id,
            OutboundMessage {
                embed: Some(build_expired_embed()),
                ..OutboundMessage::default()
            },
        );
    }

    pub fn pick<D: DiscordPort>(
        &mut self,
        view: &mut BattleHandle,
        interaction: &InteractionModel,
        move_: PetBrawlMove,
        discord: &mut D,
    ) -> PickOutcome {
        let Some(session) = self.sessions.get(&view.brawl_id) else {
            return PickOutcome::Rejected;
        };
        let side = if interaction.user_id == session.state.a.owner_id {
            Some("a")
        } else if interaction.user_id == session.state.b.owner_id {
            Some("b")
        } else {
            None
        };
        let Some(side) = side else {
            discord.initial_response(OutboundMessage::text(
                "Only the two duelists can fight this battle.",
                true,
            ));
            return PickOutcome::Rejected;
        };
        if !discord.defer(false) {
            return PickOutcome::Rejected;
        }
        if session.lock_held {
            discord.waiting_for_session_lock(view.brawl_id);
            return PickOutcome::WaitingForLock;
        }
        let session = self
            .sessions
            .get_mut(&view.brawl_id)
            .expect("session was checked above");
        if view.resolved {
            let _ignored =
                discord.followup(OutboundMessage::text("That round already resolved.", true));
            return PickOutcome::Rejected;
        }
        if session.picks.contains_key(side) {
            let _ignored = discord.followup(OutboundMessage::text(
                "You're already locked in for this round.",
                true,
            ));
            return PickOutcome::Rejected;
        }
        session.picks.insert(side, move_);
        let duelist = if side == "a" {
            &session.state.a
        } else {
            &session.state.b
        };
        let _acknowledged = discord.followup(OutboundMessage::text(
            format!(
                "{} You picked **{}** ✅ — waiting for your opponent.",
                move_.emoji(),
                move_name(&duelist.species_id, move_)
            ),
            true,
        ));
        if session.picks.len() != 2 {
            discord.edit_or_send(
                session.message_id,
                OutboundMessage {
                    embed: Some(build_battle_embed(
                        &session.state,
                        &session.last_log,
                        &session.picks,
                    )),
                    ..OutboundMessage::default()
                },
            );
            return PickOutcome::LockedIn;
        }
        session.consecutive_full_timeouts = 0;
        view.resolved = true;
        self.resolve(view.brawl_id, Vec::new(), discord)
    }

    pub fn battle_timeout<D: DiscordPort>(
        &mut self,
        view: &mut BattleHandle,
        discord: &mut D,
    ) -> PickOutcome {
        let Some(session) = self.sessions.get_mut(&view.brawl_id) else {
            return PickOutcome::Rejected;
        };
        if view.resolved || session.lock_held {
            return PickOutcome::Rejected;
        }
        let missing = ["a", "b"]
            .into_iter()
            .filter(|side| !session.picks.contains_key(side))
            .collect::<Vec<_>>();
        if missing.len() == 2 {
            session.consecutive_full_timeouts += 1;
            if session.consecutive_full_timeouts >= 2 {
                let guild_id = session.guild_id;
                let message_id = session.message_id;
                self.sessions.remove(&view.brawl_id);
                view.resolved = true;
                let _ignored = self.service.void(view.brawl_id, guild_id);
                discord.edit_or_send(
                    message_id,
                    OutboundMessage {
                        embed: Some(build_void_embed(
                            "Both camas wandered off mid-fight. The brawl is called.",
                        )),
                        ..OutboundMessage::default()
                    },
                );
                return PickOutcome::BattleFinished;
            }
        } else {
            session.consecutive_full_timeouts = 0;
        }
        let mut notes = Vec::new();
        for side in missing {
            session.picks.insert(side, SAFE_MOVE);
            let duelist = if side == "a" {
                &session.state.a
            } else {
                &session.state.b
            };
            notes.push(format!(
                "⏰ **{}** never picked — {} thrown automatically.",
                duelist.name,
                move_name(&duelist.species_id, SAFE_MOVE)
            ));
        }
        view.resolved = true;
        self.resolve(view.brawl_id, notes, discord)
    }

    fn resolve<D: DiscordPort>(
        &mut self,
        brawl_id: i64,
        prefix_log: Vec<String>,
        discord: &mut D,
    ) -> PickOutcome {
        let Some(mut session) = self.sessions.remove(&brawl_id) else {
            return PickOutcome::Rejected;
        };
        let move_a = session.picks["a"];
        let move_b = session.picks["b"];
        session.picks.clear();
        let Ok((state, round_log)) =
            resolve_round(&session.state, move_a, move_b, &mut session.rng)
        else {
            return PickOutcome::Rejected;
        };
        let mut log = prefix_log;
        log.extend(round_log);
        let rounds = state.round_no;
        session.state = state;
        session.last_log = log;
        if session.state.winner.is_none() {
            let next_view = BattleViewModel::new(brawl_id, session.state.round_no);
            discord.edit_or_send(
                session.message_id,
                OutboundMessage {
                    embed: Some(build_battle_embed(
                        &session.state,
                        &session.last_log,
                        &session.picks,
                    )),
                    view: Some(ViewModel::Battle(next_view)),
                    ..OutboundMessage::default()
                },
            );
            self.sessions.insert(brawl_id, session);
            return PickOutcome::RoundResolved;
        }
        match self.service.settle(
            brawl_id,
            session.guild_id,
            &session.state,
            i64::from(rounds),
        ) {
            ServiceResult::Success(settlement) => {
                let has_art = matches!(settlement, SettleOutcome::Decisive(_));
                discord.edit_or_send(
                    session.message_id,
                    OutboundMessage {
                        embed: Some(build_result_embed(
                            &settlement,
                            i64::from(rounds),
                            &session.last_log,
                        )),
                        attachments: if has_art {
                            vec!["pet-brawl-winner.png".to_owned()]
                        } else {
                            Vec::new()
                        },
                        ..OutboundMessage::default()
                    },
                );
            }
            ServiceResult::Failure { error, .. } => {
                let _ignored = self.service.void(brawl_id, session.guild_id);
                discord.edit_or_send(
                    session.message_id,
                    OutboundMessage {
                        embed: Some(build_void_embed(&format!(
                            "The fight ended but settling it failed: {error}"
                        ))),
                        ..OutboundMessage::default()
                    },
                );
            }
        }
        PickOutcome::BattleFinished
    }

    pub fn sweep(&mut self) {
        let _ignored = self.service.sweep_stale();
    }
}

fn tier_badge(tier: SpeciesTier) -> &'static str {
    match tier {
        SpeciesTier::Common => "",
        SpeciesTier::Uncommon => "🔹 ",
        SpeciesTier::Rare => "🔮 ",
        SpeciesTier::Legendary => "👑 ",
    }
}

#[must_use]
pub fn build_challenge_embed(
    brawl: &PetBrawl,
    challenger_pet: &Pet,
    recipient_pet: &Pet,
) -> EmbedModel {
    let challenger_species = get_species(&challenger_pet.species);
    let recipient_species = get_species(&recipient_pet.species);
    EmbedModel {
        title: "⚔️ A pet brawl challenge!".to_owned(),
        description: format!(
            "<@{}>'s {}**{}** challenges <@{}>'s {}**{}** to single combat!\n\nExpires <t:{}:R>.",
            brawl.challenger_id,
            tier_badge(challenger_species.tier),
            challenger_pet.name,
            brawl.recipient_id,
            tier_badge(recipient_species.tier),
            recipient_pet.name,
            brawl.expires_at
        ),
        footer: Some(if brawl.wager != 0 {
            format!(
                "Wager: {} {JOPACOIN_EMOTE} each · pot: {} · challenger fee: {}",
                brawl.wager,
                2 * brawl.wager,
                brawl.fee
            )
        } else {
            "No coin at stake — fullness and honor only.".to_owned()
        }),
        image_attachment: Some("pet-brawl-versus.png".to_owned()),
        color: Some(COLOR_ORANGE),
        ..EmbedModel::default()
    }
}

fn hp_bar(hp: i32, max_hp: i32) -> String {
    let filled = if max_hp <= 0 || hp <= 0 {
        0
    } else {
        let numerator = hp * 10;
        let quotient = numerator / max_hp;
        let remainder = numerator % max_hp;
        let rounded = match (2 * remainder).cmp(&max_hp) {
            std::cmp::Ordering::Less => quotient,
            std::cmp::Ordering::Greater => quotient + 1,
            std::cmp::Ordering::Equal if quotient % 2 != 0 => quotient + 1,
            std::cmp::Ordering::Equal => quotient,
        };
        rounded.clamp(0, 10)
    };
    format!(
        "{}{}",
        "█".repeat(usize::try_from(filled).expect("non-negative bar")),
        "░".repeat(usize::try_from(10 - filled).expect("non-negative bar"))
    )
}

fn duelist_line(duelist: &Duelist) -> String {
    let species = get_species(&duelist.species_id);
    format!(
        "{}**{}** ({}) — `{}` {}/{} HP",
        tier_badge(species.tier),
        duelist.name,
        species.display_name,
        hp_bar(duelist.hp, duelist.max_hp),
        duelist.hp.max(0),
        duelist.max_hp
    )
}

#[must_use]
pub fn build_battle_embed(
    state: &PetBrawlState,
    log_lines: &[String],
    picks: &BTreeMap<&'static str, PetBrawlMove>,
) -> EmbedModel {
    let mut fields = Vec::new();
    if !log_lines.is_empty() {
        fields.push(EmbedField {
            name: format!("Round {}", state.round_no),
            value: log_lines.join("\n"),
            inline: false,
        });
    }
    if state.round_no == 0 && log_lines.is_empty() {
        let move_lines = PetBrawlMove::ALL
            .iter()
            .map(|move_| {
                format!(
                    "{} **{}** — {}",
                    move_.emoji(),
                    move_name("", *move_),
                    move_.blurb()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        fields.push(EmbedField {
            name: "How it works".to_owned(),
            value: format!(
                "Both owners pick a move **every round** — picks are secret and resolve together, no turns.\n{move_lines}\nHungry camas start wounded; happy camas hit harder.\n**Training:** STR boosts attacks (especially Stampede); INT reduces incoming damage and improves Hunker; DEX boosts crits (especially Spit) and Stampede evasion."
            ),
            inline: false,
        });
        let mut quirks = Vec::new();
        for duelist in [&state.a, &state.b] {
            if let Some(blurb) = trait_blurb(&duelist.species_id) {
                quirks.push(format!("✨ **{}**: {blurb}", duelist.name));
            }
            if duelist.shield_available {
                quirks.push(format!("✨ **{}**: {SHELL_BLURB}", duelist.name));
            }
        }
        if !quirks.is_empty() {
            fields.push(EmbedField {
                name: "Quirks".to_owned(),
                value: quirks.join("\n"),
                inline: false,
            });
        }
    }
    let lock_a = if picks.contains_key("a") {
        "✅"
    } else {
        "🤔"
    };
    let lock_b = if picks.contains_key("b") {
        "✅"
    } else {
        "🤔"
    };
    EmbedModel {
        title: format!("⚔️ Pet Brawl — Round {}/{}", state.round_no + 1, MAX_ROUNDS),
        description: format!("{}\n{}", duelist_line(&state.a), duelist_line(&state.b)),
        fields,
        footer: Some(format!(
            "{} {lock_a} · {} {lock_b} — {PET_BRAWL_TURN_SECONDS}s per move, picks are secret",
            state.a.name, state.b.name
        )),
        color: Some(COLOR_ORANGE),
        ..EmbedModel::default()
    }
}

#[must_use]
pub fn build_result_embed(
    settlement: &SettleOutcome,
    rounds: i64,
    final_log: &[String],
) -> EmbedModel {
    match settlement {
        SettleOutcome::Draw(draw) => build_draw_embed(draw, rounds, final_log),
        SettleOutcome::Decisive(decisive) => build_decisive_embed(decisive, rounds, final_log),
    }
}

fn build_draw_embed(draw: &DrawOutcome, rounds: i64, final_log: &[String]) -> EmbedModel {
    let (a, b) = &draw.duelists;
    let record_a = draw.records.get(&a.pet_id).copied().unwrap_or_default();
    let record_b = draw.records.get(&b.pet_id).copied().unwrap_or_default();
    let log = if final_log.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", final_log.join("\n"))
    };
    EmbedModel {
        title: "🤝 The brawl ends in a draw!".to_owned(),
        description: format!(
            "{log}Draw after {rounds} rounds.\nNo fullness, records, or training XP change.\n\n⚔️ **{}** {}W-{}L · **{}** {}W-{}L",
            a.name, record_a.0, record_a.1, b.name, record_b.0, record_b.1
        ),
        footer: Some(if draw.wager != 0 {
            format!(
                "Wagers refunded: {} JC each · challenger fee: {} JC returned.",
                draw.wager, draw.fee
            )
        } else {
            "No jopacoins change hands — the honors finish even.".to_owned()
        }),
        color: Some(COLOR_ORANGE),
        ..EmbedModel::default()
    }
}

fn build_decisive_embed(
    decisive: &DecisiveOutcome,
    rounds: i64,
    final_log: &[String],
) -> EmbedModel {
    let winner = &decisive.winner;
    let loser = &decisive.loser;
    let winner_gain = if decisive.winner_delta > 0 {
        format!(
            "**{}** feasts on victory: fullness +{}",
            winner.name, decisive.winner_delta
        )
    } else {
        format!("**{}** gets no fullness from this victory", winner.name)
    };
    let loser_loss = if decisive.loser_delta < 0 {
        format!(
            "**{}** sulks off: fullness {}",
            loser.name, decisive.loser_delta
        )
    } else {
        format!("**{}** loses no fullness from this defeat", loser.name)
    };
    let win_record = decisive
        .records
        .get(&winner.pet_id)
        .copied()
        .unwrap_or_default();
    let loss_record = decisive
        .records
        .get(&loser.pet_id)
        .copied()
        .unwrap_or_default();
    let mut extras = vec![
        progression_line(
            winner,
            decisive.winner_xp_delta,
            decisive.winner_stat_gain.as_deref(),
            decisive.career.get(&winner.pet_id),
        ),
        progression_line(
            loser,
            decisive.loser_xp_delta,
            decisive.loser_stat_gain.as_deref(),
            decisive.career.get(&loser.pet_id),
        ),
    ];
    if decisive.wager != 0 {
        extras.push(format!(
            "💰 Pot: **{}** {JOPACOIN_EMOTE} · fee: {} to the Reserve",
            decisive.payout, decisive.fee
        ));
    }
    if let Some(event) = &decisive.personality_event {
        extras.push(event.clone());
    }
    let log = if final_log.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", final_log.join("\n"))
    };
    let round_word = if rounds == 1 { "round" } else { "rounds" };
    EmbedModel {
        title: format!("🏆 {} wins the brawl!", winner.name),
        description: format!(
            "{log}Victory in {rounds} {round_word}.\n{winner_gain}\n{loser_loss}\n\n⚔️ **{}** {}W-{}L · **{}** {}W-{}L\n\n{}",
            winner.name,
            win_record.0,
            win_record.1,
            loser.name,
            loss_record.0,
            loss_record.1,
            extras.join("\n")
        ),
        footer: Some(if decisive.wager != 0 {
            format!(
                "Wager settled: {} JC to the winner · {} JC fee to the Reserve.",
                decisive.payout, decisive.fee
            )
        } else {
            "No jopacoins change hands — brawls play for fullness and honor.".to_owned()
        }),
        image_attachment: Some("pet-brawl-winner.png".to_owned()),
        color: Some(COLOR_GREEN),
        ..EmbedModel::default()
    }
}

fn progression_line(
    duelist: &Duelist,
    xp_delta: i64,
    stat_gain: Option<&str>,
    career: Option<&crate::pet_brawl::CareerSummary>,
) -> String {
    let mut parts = vec![format!(
        "**{}** +{xp_delta} XP ({}/20)",
        duelist.name,
        career.map_or(0, |summary| summary.xp)
    )];
    if let Some(stat_gain) = stat_gain {
        parts.push(format!("{} +1", stat_gain.to_uppercase()));
    }
    if let Some(career) = career {
        let title = [career.rank.as_deref(), career.build.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        if !title.is_empty() {
            parts.push(title);
        }
    }
    parts.join(" · ")
}

#[must_use]
pub fn build_training_field(
    career: &crate::pet_brawl::CareerSummary,
    solo_training_sessions: i64,
) -> EmbedField {
    let titles = [career.rank.as_deref(), career.build.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let level = career.strength + career.intelligence + career.dexterity;
    let title_line = if titles.is_empty() {
        String::new()
    } else {
        format!("\n{titles}")
    };
    EmbedField {
        name: "🏋️ Training".to_owned(),
        value: format!(
            "Level {level}/4 · {}/20 XP\nSTR {} · INT {} · DEX {}\nSolo: {solo_training_sessions}/3 banked · `/pet train`{title_line}",
            career.xp, career.strength, career.intelligence, career.dexterity
        ),
        inline: false,
    }
}

#[must_use]
pub fn build_declined_embed(recipient_id: i64) -> EmbedModel {
    EmbedModel {
        title: "🐫 Challenge declined".to_owned(),
        description: format!("<@{recipient_id}> politely declines. The camas graze on."),
        color: Some(COLOR_BLUE),
        ..EmbedModel::default()
    }
}

#[must_use]
pub fn build_expired_embed() -> EmbedModel {
    EmbedModel {
        title: "🌾 Challenge expired".to_owned(),
        description: "Nobody answered the call. The arena stands empty.".to_owned(),
        color: Some(COLOR_DEAD),
        ..EmbedModel::default()
    }
}

#[must_use]
pub fn build_void_embed(reason: &str) -> EmbedModel {
    EmbedModel {
        title: "🌪️ Brawl called off".to_owned(),
        description: reason.to_owned(),
        color: Some(COLOR_DEAD),
        ..EmbedModel::default()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use cama_domain::pet::{PetBase, PetStage};
    use cama_domain::pet_brawl::{
        BrawlWinner, DuelistSnapshot, PetBrawlSnapshot, build_duelist, initial_state,
    };

    use super::*;
    use crate::pet_brawl::CareerSummary;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 7;
    const CHALLENGER: i64 = 100;
    const RECIPIENT: i64 = 200;
    const STRANGER: i64 = 999;
    const PET_CHANNEL: i64 = 777_000;

    #[derive(Clone, Debug)]
    struct FakeService {
        challenger_pet: Pet,
        recipient_pet: Pet,
        brawl: PetBrawl,
        status: String,
        balances: BTreeMap<i64, i64>,
        settle_failure: bool,
        void_calls: usize,
        sweep_calls: usize,
    }

    impl Default for FakeService {
        fn default() -> Self {
            Self {
                challenger_pet: pet(1, CHALLENGER, "Brawler", "common_cama"),
                recipient_pet: pet(2, RECIPIENT, "Brawler", "common_cama"),
                brawl: brawl(0),
                status: "pending".to_owned(),
                balances: BTreeMap::from([(CHALLENGER, 100), (RECIPIENT, 100)]),
                settle_failure: false,
                void_calls: 0,
                sweep_calls: 0,
            }
        }
    }

    impl PetBrawlApplication for FakeService {
        fn train_solo(
            &mut self,
            _discord_id: i64,
            _guild_id: Option<i64>,
        ) -> ServiceResult<SoloTrainingOutcome> {
            self.challenger_pet.training_xp = 3;
            self.challenger_pet.solo_training_sessions = 0;
            ServiceResult::ok(SoloTrainingOutcome {
                sessions_used: 3,
                sessions_available: 0,
                xp_delta: 3,
                stat_gain: None,
                pet: self.challenger_pet.clone(),
                career: CareerSummary {
                    xp: 3,
                    strength: 0,
                    intelligence: 0,
                    dexterity: 0,
                    rank: None,
                    build: None,
                },
                flavor: "🏃 **Brawler** tears through a dusty obstacle course.".to_owned(),
            })
        }

        fn challenge(
            &mut self,
            challenger_id: i64,
            recipient_id: i64,
            _guild_id: Option<i64>,
            channel_id: i64,
            wager: WagerInput,
        ) -> ServiceResult<ChallengeOutcome> {
            let WagerInput::Integer(wager) = wager else {
                return ServiceResult::fail("invalid wager");
            };
            let fee = if wager == 0 { 0 } else { 1 };
            self.brawl = brawl(wager);
            self.brawl.channel_id = channel_id;
            self.brawl.challenger_id = challenger_id;
            self.brawl.recipient_id = recipient_id;
            self.brawl.fee = fee;
            self.status = "pending".to_owned();
            self.balances.insert(challenger_id, 100 - wager - fee);
            ServiceResult::ok(ChallengeOutcome {
                brawl: self.brawl.clone(),
                challenger_pet: self.challenger_pet.clone(),
                recipient_pet: self.recipient_pet.clone(),
            })
        }

        fn accept(
            &mut self,
            _brawl_id: i64,
            _guild_id: Option<i64>,
            _recipient_id: i64,
        ) -> ServiceResult<AcceptOutcome> {
            self.status = "active".to_owned();
            self.brawl.status = "active".to_owned();
            self.brawl.recipient_pet_id = Some(self.recipient_pet.pet_id);
            self.balances.insert(RECIPIENT, 100 - self.brawl.wager);
            ServiceResult::ok(AcceptOutcome {
                brawl: self.brawl.clone(),
                state: initial_state(
                    pet_duelist(&self.challenger_pet),
                    pet_duelist(&self.recipient_pet),
                ),
                combat_seed: 42,
            })
        }

        fn decline(
            &mut self,
            _brawl_id: i64,
            _guild_id: Option<i64>,
            _recipient_id: i64,
        ) -> ServiceResult<()> {
            self.status = "declined".to_owned();
            ServiceResult::ok(())
        }

        fn withdraw(
            &mut self,
            brawl_id: i64,
            guild_id: Option<i64>,
            _challenger_id: i64,
        ) -> ServiceResult<()> {
            self.void(brawl_id, guild_id)
        }

        fn void(&mut self, _brawl_id: i64, _guild_id: Option<i64>) -> ServiceResult<()> {
            self.status = "void".to_owned();
            self.void_calls += 1;
            self.balances.insert(CHALLENGER, 100);
            self.balances.insert(RECIPIENT, 100);
            ServiceResult::ok(())
        }

        fn settle(
            &mut self,
            _brawl_id: i64,
            _guild_id: Option<i64>,
            final_state: &PetBrawlState,
            _rounds: i64,
        ) -> ServiceResult<SettleOutcome> {
            if self.settle_failure {
                return ServiceResult::fail("database unavailable");
            }
            self.status = "done".to_owned();
            if final_state.winner == Some(BrawlWinner::Draw) {
                self.balances.insert(CHALLENGER, 100);
                self.balances.insert(RECIPIENT, 100);
                return ServiceResult::ok(SettleOutcome::Draw(DrawOutcome {
                    duelists: (final_state.a.clone(), final_state.b.clone()),
                    records: BTreeMap::new(),
                    wager: self.brawl.wager,
                    fee: self.brawl.fee,
                }));
            }
            let (winner, loser) = if final_state.winner == Some(BrawlWinner::A) {
                (&final_state.a, &final_state.b)
            } else {
                (&final_state.b, &final_state.a)
            };
            let records = BTreeMap::from([(winner.pet_id, (1, 0)), (loser.pet_id, (0, 1))]);
            let career = BTreeMap::from([
                (
                    winner.pet_id,
                    CareerSummary {
                        xp: 2,
                        strength: 0,
                        intelligence: 0,
                        dexterity: 0,
                        rank: Some("Rookie".to_owned()),
                        build: None,
                    },
                ),
                (
                    loser.pet_id,
                    CareerSummary {
                        xp: 1,
                        strength: 0,
                        intelligence: 0,
                        dexterity: 0,
                        rank: Some("Rookie".to_owned()),
                        build: None,
                    },
                ),
            ]);
            ServiceResult::ok(SettleOutcome::Decisive(DecisiveOutcome {
                winner: winner.clone(),
                loser: loser.clone(),
                winner_delta: 10,
                loser_delta: -15,
                winner_xp_delta: 2,
                loser_xp_delta: 1,
                winner_stat_gain: None,
                loser_stat_gain: None,
                payout: self.brawl.wager * 2,
                wager: self.brawl.wager,
                fee: self.brawl.fee,
                records,
                career,
                personality_event_key: None,
                personality_event: None,
            }))
        }

        fn sweep_stale(&mut self) -> PortResult<SweepResult> {
            self.sweep_calls += 1;
            Ok(SweepResult {
                expired: 0,
                voided: 0,
            })
        }
    }

    fn pet(pet_id: i64, discord_id: i64, name: &str, species: &str) -> Pet {
        Pet::new(PetBase {
            pet_id,
            discord_id,
            guild_id: GUILD,
            name: name.to_owned(),
            species: species.to_owned(),
            adopted_at: NOW - 10 * DAY,
            hatched_at: NOW - 9 * DAY,
            adopt_fee: 20,
            last_fed_at: NOW,
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
            dig_work_at: NOW,
            aegis_used: 0,
            hatch_announced_at: None,
            died_at: None,
            death_cause: None,
            death_announced_at: None,
        })
    }

    fn pet_duelist(pet: &Pet) -> Duelist {
        build_duelist(DuelistSnapshot {
            pet_id: pet.pet_id,
            owner_id: pet.discord_id,
            name: &pet.name,
            species_id: &pet.species,
            stage: PetStage::Adult,
            hunger: 100,
            happy: false,
            aegis_used: pet.aegis_used,
            training_str: i32::try_from(pet.training_str).expect("test training fits"),
            training_int: i32::try_from(pet.training_int).expect("test training fits"),
            training_dex: i32::try_from(pet.training_dex).expect("test training fits"),
        })
    }

    fn brawl(wager: i64) -> PetBrawl {
        let mut value = PetBrawl::from(PetBrawlSnapshot {
            brawl_id: 1,
            guild_id: GUILD,
            channel_id: PET_CHANNEL,
            challenger_id: CHALLENGER,
            recipient_id: RECIPIENT,
            challenger_pet_id: 1,
            recipient_pet_id: None,
            status: "pending",
            created_at: NOW,
            expires_at: NOW + 180,
            resolved_at: None,
            winner_id: None,
            winner_pet_id: None,
            loser_pet_id: None,
            rounds: None,
            winner_hunger_delta: 0,
            loser_hunger_delta: 0,
        });
        value.wager = wager;
        value.fee = i64::from(wager != 0);
        value
    }

    fn duelist(species: &str, name: &str, owner_id: i64, pet_id: i64) -> Duelist {
        build_duelist(DuelistSnapshot {
            pet_id,
            owner_id,
            name,
            species_id: species,
            stage: PetStage::Adult,
            hunger: 100,
            happy: false,
            aegis_used: 0,
            training_str: 0,
            training_int: 0,
            training_dex: 0,
        })
    }

    fn interaction(user_id: i64, channel_id: i64) -> InteractionModel {
        InteractionModel {
            user_id,
            display_name: "Owner".to_owned(),
            is_bot: false,
            guild_id: Some(GUILD),
            channel_id,
            parent_channel_id: None,
            rate_allowed: true,
            retry_after_seconds: 0,
        }
    }

    fn setup() -> (PetBrawlCommands<FakeService>, InMemoryDiscord) {
        (
            PetBrawlCommands::new(FakeService::default(), PET_CHANNEL),
            InMemoryDiscord::default(),
        )
    }

    fn issue_challenge(
        app: &mut PetBrawlCommands<FakeService>,
        discord: &mut InMemoryDiscord,
        wager: Option<i64>,
    ) -> ChallengeHandle {
        app.brawl(
            &interaction(CHALLENGER, PET_CHANNEL),
            TargetMember {
                id: RECIPIENT,
                is_bot: false,
            },
            wager,
            discord,
        )
        .expect("challenge should post")
    }

    fn start_battle(
        app: &mut PetBrawlCommands<FakeService>,
        discord: &mut InMemoryDiscord,
        wager: Option<i64>,
    ) -> BattleHandle {
        let mut challenge = issue_challenge(app, discord, wager);
        app.accept(
            &mut challenge,
            &interaction(RECIPIENT, PET_CHANNEL),
            discord,
        )
        .expect("recipient should start battle")
    }

    fn last_embed(discord: &InMemoryDiscord) -> &EmbedModel {
        discord
            .last_message()
            .and_then(|message| message.embed.as_ref())
            .expect("latest message has embed")
    }

    fn last_content(discord: &InMemoryDiscord) -> &str {
        discord
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                DiscordEvent::Initial(message) | DiscordEvent::FollowupAttempt(message) => {
                    message.content.as_deref()
                }
                DiscordEvent::Edited { message, .. } => message.content.as_deref(),
                DiscordEvent::Deferred { .. } | DiscordEvent::WaitingForSessionLock { .. } => None,
            })
            .expect("latest message has content")
    }

    #[test]
    fn test_training_cashes_in_banked_sessions() {
        let (mut app, mut discord) = setup();

        app.train(&interaction(CHALLENGER, PET_CHANNEL), &mut discord);

        assert!(last_content(&discord).contains("3 banked sessions"));
        assert!(last_content(&discord).contains("+3 XP"));
        assert!(
            discord
                .last_message()
                .is_some_and(|message| message.ephemeral)
        );
        assert_eq!(app.service().challenger_pet.training_xp, 3);
        assert_eq!(app.service().challenger_pet.solo_training_sessions, 0);
    }

    #[test]
    fn test_happy_path_posts_challenge() {
        let (mut app, mut discord) = setup();

        let challenge = issue_challenge(&mut app, &mut discord, None);

        assert!(
            last_embed(&discord)
                .title
                .to_lowercase()
                .contains("challenge")
        );
        assert_eq!(
            last_embed(&discord).footer.as_deref(),
            Some("No coin at stake — fullness and honor only.")
        );
        assert_eq!(app.service().status, "pending");
        assert_eq!(challenge.brawl.status, "pending");
    }

    #[test]
    fn test_optional_wager_posts_stakes_and_escrows_challenger() {
        let (mut app, mut discord) = setup();

        let challenge = issue_challenge(&mut app, &mut discord, Some(20));

        let footer = last_embed(&discord).footer.as_deref().unwrap_or_default();
        assert!(footer.contains("20"));
        assert!(footer.contains("40"));
        assert!(footer.to_lowercase().contains("fee: 1"));
        assert_eq!(app.service().balances[&CHALLENGER], 79);
        assert_eq!(challenge.brawl.wager, 20);
    }

    #[test]
    fn test_failed_challenge_delivery_voids_and_refunds() {
        let (mut app, mut discord) = setup();
        discord.deliver_followups = false;

        let challenge = app.brawl(
            &interaction(CHALLENGER, PET_CHANNEL),
            TargetMember {
                id: RECIPIENT,
                is_bot: false,
            },
            Some(20),
            &mut discord,
        );

        assert!(challenge.is_none());
        assert_eq!(app.service().status, "void");
        assert_eq!(app.service().balances[&CHALLENGER], 100);
    }

    #[test]
    fn test_rejected_outside_pet_channel() {
        let (mut app, mut discord) = setup();

        let challenge = app.brawl(
            &interaction(CHALLENGER, 123),
            TargetMember {
                id: RECIPIENT,
                is_bot: false,
            },
            None,
            &mut discord,
        );

        assert!(challenge.is_none());
        assert!(last_content(&discord).contains("arena"));
        assert_eq!(discord.defer_count(), 0);
    }

    #[test]
    fn test_rejects_bot_target() {
        let (mut app, mut discord) = setup();

        let challenge = app.brawl(
            &interaction(CHALLENGER, PET_CHANNEL),
            TargetMember {
                id: RECIPIENT,
                is_bot: true,
            },
            None,
            &mut discord,
        );

        assert!(challenge.is_none());
        assert!(last_content(&discord).contains("Bots"));
    }

    #[test]
    fn test_status_embed_shows_training_stats_rank_and_build() {
        let career = CareerSummary {
            xp: 20,
            strength: 2,
            intelligence: 1,
            dexterity: 1,
            rank: Some("Champion".to_owned()),
            build: Some("Bruiser".to_owned()),
        };

        let training = build_training_field(&career, 3);

        assert!(training.value.contains("20/20 XP"));
        assert!(training.value.contains("STR 2"));
        assert!(training.value.contains("INT 1"));
        assert!(training.value.contains("DEX 1"));
        assert!(training.value.contains("Champion"));
        assert!(training.value.contains("Bruiser"));
        assert!(training.value.contains("3/3 banked"));
    }

    #[test]
    fn test_accept_recipient_only() {
        let (mut app, mut discord) = setup();
        let mut challenge = issue_challenge(&mut app, &mut discord, None);

        let battle = app.accept(
            &mut challenge,
            &interaction(STRANGER, PET_CHANNEL),
            &mut discord,
        );

        assert!(battle.is_none());
        assert!(last_content(&discord).contains("isn't yours"));
        assert_eq!(app.service().status, "pending");
    }

    #[test]
    fn test_accept_starts_battle() {
        let (mut app, mut discord) = setup();
        let mut challenge = issue_challenge(&mut app, &mut discord, None);

        let battle = app
            .accept(
                &mut challenge,
                &interaction(RECIPIENT, PET_CHANNEL),
                &mut discord,
            )
            .expect("accept starts battle");

        assert_eq!(app.service().status, "active");
        assert!(app.has_session(challenge.brawl.brawl_id));
        assert!(matches!(
            discord
                .last_message()
                .and_then(|message| message.view.as_ref()),
            Some(ViewModel::Battle(_))
        ));
        assert!(last_embed(&discord).title.contains("Round 1"));
        assert!(last_embed(&discord).field("How it works").is_some());
        assert_eq!(battle.round_no, 0);
    }

    #[test]
    fn test_decline() {
        let (mut app, mut discord) = setup();
        let mut challenge = issue_challenge(&mut app, &mut discord, None);

        app.decline(
            &mut challenge,
            &interaction(RECIPIENT, PET_CHANNEL),
            &mut discord,
        );

        assert_eq!(app.service().status, "declined");
        assert!(last_embed(&discord).title.contains("declined"));
    }

    #[test]
    fn test_withdraw_challenger_only() {
        let (mut app, mut discord) = setup();
        let mut challenge = issue_challenge(&mut app, &mut discord, None);

        app.withdraw(
            &mut challenge,
            &interaction(RECIPIENT, PET_CHANNEL),
            &mut discord,
        );
        assert!(last_content(&discord).contains("challenger"));
        app.withdraw(
            &mut challenge,
            &interaction(CHALLENGER, PET_CHANNEL),
            &mut discord,
        );

        assert_eq!(app.service().status, "void");
    }

    #[test]
    fn test_timeout_voids() {
        let (mut app, mut discord) = setup();
        let mut challenge = issue_challenge(&mut app, &mut discord, None);

        app.challenge_timeout(&mut challenge, &mut discord);

        assert_eq!(app.service().status, "void");
        assert!(last_embed(&discord).title.contains("expired"));
    }

    #[test]
    fn test_battle_offers_five_moves_and_explains_training_stats() {
        let (mut app, mut discord) = setup();

        let view = start_battle(&mut app, &mut discord, None);

        let view_model = view.view_model();
        let offered = view_model
            .custom_ids
            .iter()
            .map(|custom_id| custom_id.rsplit(':').next().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(offered, ["spit", "stampede", "hunker", "feint", "sidestep"]);
        let rules = &last_embed(&discord)
            .field("How it works")
            .expect("primer exists")
            .value;
        for term in ["Feint", "Sidestep", "STR", "INT", "DEX"] {
            assert!(rules.contains(term));
        }
    }

    #[test]
    fn test_stranger_cannot_fight() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);

        let outcome = app.pick(
            &mut view,
            &interaction(STRANGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        assert_eq!(outcome, PickOutcome::Rejected);
        assert!(last_content(&discord).contains("duelists"));
    }

    #[test]
    fn test_double_click_guard() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);

        assert_eq!(
            app.pick(
                &mut view,
                &interaction(CHALLENGER, PET_CHANNEL),
                SAFE_MOVE,
                &mut discord,
            ),
            PickOutcome::LockedIn
        );
        assert!(last_content(&discord).contains("picked"));
        assert_eq!(
            app.pick(
                &mut view,
                &interaction(CHALLENGER, PET_CHANNEL),
                PetBrawlMove::Hunker,
                &mut discord,
            ),
            PickOutcome::Rejected
        );
        assert!(last_content(&discord).contains("already locked"));
        assert_eq!(app.session_picks(view.brawl_id).unwrap()["a"], SAFE_MOVE);
    }

    #[test]
    fn test_both_picks_resolve_round() {
        let mut python_rng = PythonCombatRng::new(42);
        let replay = (0..12)
            .map(|_| python_rng.randint(1, 100))
            .collect::<Vec<_>>();
        assert_eq!(replay, [82, 15, 4, 95, 36, 32, 29, 18, 95, 14, 87, 95]);
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);

        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        let second = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        assert_eq!(second, PickOutcome::RoundResolved);
        assert_eq!(app.session_state(view.brawl_id).unwrap().round_no, 1);
        assert!(app.session_picks(view.brawl_id).unwrap().is_empty());
        assert!(last_embed(&discord).title.contains("Round 2"));
        assert!(matches!(
            discord
                .last_message()
                .and_then(|message| message.view.as_ref()),
            Some(ViewModel::Battle(_))
        ));
    }

    #[test]
    fn test_timeout_autopicks_for_laggard_only() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);

        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            PetBrawlMove::Hunker,
            &mut discord,
        );
        let outcome = app.battle_timeout(&mut view, &mut discord);

        assert_eq!(outcome, PickOutcome::RoundResolved);
        assert_eq!(app.session_state(view.brawl_id).unwrap().round_no, 1);
        assert_eq!(app.session_full_timeouts(view.brawl_id), Some(0));
        let log = app.session_last_log(view.brawl_id).unwrap();
        assert!(log.iter().any(|line| line.contains("hunkers")));
        assert!(log.iter().any(|line| line.contains("never picked")));
    }

    #[test]
    fn test_double_full_timeout_voids() {
        let (mut app, mut discord) = setup();
        let mut first_view = start_battle(&mut app, &mut discord, None);

        assert_eq!(
            app.battle_timeout(&mut first_view, &mut discord),
            PickOutcome::RoundResolved
        );
        assert_eq!(app.session_full_timeouts(first_view.brawl_id), Some(1));
        assert_eq!(app.session_state(first_view.brawl_id).unwrap().round_no, 1);
        let mut next_view = BattleHandle {
            brawl_id: first_view.brawl_id,
            message_id: first_view.message_id,
            round_no: 1,
            resolved: false,
        };
        assert_eq!(
            app.battle_timeout(&mut next_view, &mut discord),
            PickOutcome::BattleFinished
        );

        assert_eq!(app.service().status, "void");
        assert!(!app.has_session(first_view.brawl_id));
    }

    #[test]
    fn test_knockout_settles_and_reports() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);
        let mut state = app.session_state(view.brawl_id).unwrap().clone();
        state.b.hp = 1;
        app.set_session_state(view.brawl_id, state);

        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        let outcome = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        assert_eq!(outcome, PickOutcome::BattleFinished);
        assert_eq!(app.service().status, "done");
        assert!(!app.has_session(view.brawl_id));
        assert!(last_embed(&discord).title.contains("wins the brawl"));
        assert!(last_embed(&discord).description.contains("XP"));
        assert!(last_embed(&discord).description.contains("Rookie"));
        assert!(
            discord
                .last_message()
                .is_some_and(|message| message.view.is_none())
        );
        assert!(
            last_embed(&discord)
                .footer
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("jopacoin")
        );
    }

    #[test]
    fn test_wagered_result_footer_reports_the_payout() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, Some(20));
        let mut state = app.session_state(view.brawl_id).unwrap().clone();
        state.b.hp = 1;
        app.set_session_state(view.brawl_id, state);

        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        let _second = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        let footer = last_embed(&discord).footer.as_deref().unwrap_or_default();
        assert!(footer.contains("40"));
        assert!(!footer.to_lowercase().contains("no jopacoins"));
    }

    #[test]
    fn test_equal_hp_round_cap_reports_draw_and_refunds_wager() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, Some(20));
        let mut state = app.session_state(view.brawl_id).unwrap().clone();
        state.round_no = MAX_ROUNDS - 1;
        app.set_session_state(view.brawl_id, state);

        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            PetBrawlMove::Hunker,
            &mut discord,
        );
        let outcome = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            PetBrawlMove::Hunker,
            &mut discord,
        );

        assert_eq!(outcome, PickOutcome::BattleFinished);
        assert_eq!(app.service().status, "done");
        assert!(!app.has_session(view.brawl_id));
        assert!(last_embed(&discord).title.to_lowercase().contains("draw"));
        assert!(
            last_embed(&discord)
                .footer
                .as_deref()
                .unwrap_or_default()
                .to_lowercase()
                .contains("refunded")
        );
        assert_eq!(app.service().balances[&CHALLENGER], 100);
        assert_eq!(app.service().balances[&RECIPIENT], 100);
    }

    #[test]
    fn test_round_resolves_even_if_pick_ack_fails() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);
        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        discord.fail_next_followup = true;

        let outcome = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        assert_eq!(outcome, PickOutcome::RoundResolved);
        assert_eq!(app.session_state(view.brawl_id).unwrap().round_no, 1);
        assert!(app.session_picks(view.brawl_id).unwrap().is_empty());
    }

    #[test]
    fn test_round_views_use_distinct_custom_ids() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);
        let old_ids = view.view_model().custom_ids;
        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        let _second = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        let new_ids = match discord
            .last_message()
            .and_then(|message| message.view.as_ref())
        {
            Some(ViewModel::Battle(view)) => &view.custom_ids,
            _ => panic!("next round view missing"),
        };

        assert!(old_ids.iter().all(|old| !new_ids.contains(old)));
    }

    #[test]
    fn test_click_is_acked_while_round_resolves() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, None);
        app.set_session_lock_held(view.brawl_id, true);
        let events_before = discord.events.len();

        let outcome = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        assert_eq!(outcome, PickOutcome::WaitingForLock);
        assert!(matches!(
            discord.events.get(events_before),
            Some(DiscordEvent::Deferred { .. })
        ));
        assert!(matches!(
            discord.events.get(events_before + 1),
            Some(DiscordEvent::WaitingForSessionLock { .. })
        ));
        app.set_session_lock_held(view.brawl_id, false);
        assert_eq!(
            app.pick(
                &mut view,
                &interaction(CHALLENGER, PET_CHANNEL),
                SAFE_MOVE,
                &mut discord,
            ),
            PickOutcome::LockedIn
        );
        assert!(last_content(&discord).contains("picked"));
    }

    #[test]
    fn test_quirks_field_lists_traits_and_shell() {
        let state = initial_state(
            duelist("rama", "Spitfire", CHALLENGER, 1),
            duelist("aegis_cama", "Shelly", RECIPIENT, 2),
        );

        let embed = build_battle_embed(&state, &[], &BTreeMap::new());
        let quirks = &embed.field("Quirks").expect("quirks field").value;

        assert!(quirks.contains("Spitfire") && quirks.contains("crits"));
        assert!(quirks.contains("Shelly") && quirks.contains("1 HP"));
    }

    #[test]
    fn test_quirkless_pair_gets_rules_but_no_quirks_field() {
        let state = initial_state(
            duelist("common_cama", "Plain", CHALLENGER, 1),
            duelist("common_cama", "Simple", RECIPIENT, 2),
        );

        let embed = build_battle_embed(&state, &[], &BTreeMap::new());

        assert!(embed.field("How it works").is_some());
        assert!(embed.field("Quirks").is_none());
    }

    #[test]
    fn test_jopacama_quirk_describes_fullness_insurance() {
        let state = initial_state(
            duelist("jopacama", "Gilded", CHALLENGER, 1),
            duelist("common_cama", "Plain", RECIPIENT, 2),
        );

        let embed = build_battle_embed(&state, &[], &BTreeMap::new());
        let quirks = &embed.field("Quirks").expect("quirks field").value;

        assert!(quirks.contains("loses less fullness in defeat"));
        assert!(!quirks.contains("hunger"));
    }

    #[test]
    fn test_result_describes_fullness_stakes() {
        let winner = duelist("common_cama", "Winner", CHALLENGER, 1);
        let loser = duelist("common_cama", "Loser", RECIPIENT, 2);
        let settlement = decisive(winner, loser, 10, -15, 0, 0);

        let embed = build_result_embed(&SettleOutcome::Decisive(settlement), 3, &[]);

        assert!(embed.description.contains("fullness +10"));
        assert!(embed.description.contains("fullness -15"));
        assert!(
            embed
                .footer
                .as_deref()
                .unwrap_or_default()
                .contains("fullness and honor")
        );
    }

    #[test]
    fn test_zero_delta_result_does_not_guess_why_fullness_was_unchanged() {
        let winner = duelist("common_cama", "Winner", CHALLENGER, 1);
        let loser = duelist("common_cama", "Loser", RECIPIENT, 2);
        let settlement = decisive(winner, loser, 0, 0, 0, 0);

        let embed = build_result_embed(&SettleOutcome::Decisive(settlement), 3, &[]);

        assert!(
            embed
                .description
                .contains("gets no fullness from this victory")
        );
        assert!(
            embed
                .description
                .contains("loses no fullness from this defeat")
        );
        assert!(!embed.description.contains("too well-fed"));
        assert!(!embed.description.contains("too hungry"));
    }

    #[test]
    fn test_draw_result_describes_unchanged_fullness() {
        let a = duelist("common_cama", "First", CHALLENGER, 1);
        let b = duelist("common_cama", "Second", RECIPIENT, 2);
        let settlement = SettleOutcome::Draw(DrawOutcome {
            duelists: (a, b),
            records: BTreeMap::new(),
            wager: 0,
            fee: 0,
        });

        let embed = build_result_embed(&settlement, 8, &[]);

        assert!(
            embed
                .description
                .contains("No fullness, records, or training XP change.")
        );
        assert!(!embed.description.to_lowercase().contains("hunger"));
    }

    #[test]
    fn test_failed_settlement_immediately_voids_and_refunds_wager() {
        let (mut app, mut discord) = setup();
        let mut view = start_battle(&mut app, &mut discord, Some(20));
        let mut state = app.session_state(view.brawl_id).unwrap().clone();
        state.b.hp = 1;
        app.set_session_state(view.brawl_id, state);
        app.service_mut().settle_failure = true;

        let _first = app.pick(
            &mut view,
            &interaction(CHALLENGER, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );
        let outcome = app.pick(
            &mut view,
            &interaction(RECIPIENT, PET_CHANNEL),
            SAFE_MOVE,
            &mut discord,
        );

        assert_eq!(outcome, PickOutcome::BattleFinished);
        assert_eq!(app.service().status, "void");
        assert_eq!(
            (
                app.service().balances[&CHALLENGER],
                app.service().balances[&RECIPIENT]
            ),
            (100, 100)
        );
        assert!(
            last_embed(&discord)
                .description
                .contains("database unavailable")
        );
    }

    #[test]
    fn test_sweep_loop_calls_brawl_sweep() {
        let (mut app, _discord) = setup();

        app.sweep();

        assert_eq!(app.service().sweep_calls, 1);
    }

    fn decisive(
        winner: Duelist,
        loser: Duelist,
        winner_delta: i64,
        loser_delta: i64,
        wager: i64,
        fee: i64,
    ) -> DecisiveOutcome {
        DecisiveOutcome {
            winner,
            loser,
            winner_delta,
            loser_delta,
            winner_xp_delta: 0,
            loser_xp_delta: 0,
            winner_stat_gain: None,
            loser_stat_gain: None,
            payout: wager * 2,
            wager,
            fee,
            records: BTreeMap::new(),
            career: BTreeMap::new(),
            personality_event_key: None,
            personality_event: None,
        }
    }
}
