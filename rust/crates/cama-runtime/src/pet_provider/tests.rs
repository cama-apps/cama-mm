use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::test_support::initialize_test_database as initialize_or_migrate;
use cama_app::pet::{Clock, SeededPetRandom, SystemPetClock};
use cama_app::pet_sqlite::SqlitePetCommandService;
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::pet_repository::PetRepository;
use cama_domain::pet::{ADULT_AGE_SECONDS, EGG_HATCH_SECONDS, UNHATCHED_SPECIES};
use tempfile::NamedTempFile;

use super::*;
use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt,
    DiscordMessageSnapshot, DiscordTransport,
};
use crate::registration::{
    InteractionAttachmentPolicy, InteractionMessageDelivery, InteractionResponseError,
};
use crate::reminder_provider::ReminderRegistrationProvider;
use crate::serenity_transport::SerenityDiscordTransport;

const OWNER: u64 = 77_001;
const RECIPIENT: u64 = 77_004;
const GUILD: u64 = 77_002;
const CHANNEL: u64 = 77_003;

struct TestResponder {
    fail_update: AtomicBool,
    fail_receipt: AtomicBool,
    defers: StdMutex<Vec<bool>>,
    responses: StdMutex<Vec<InteractionResponse>>,
    followups: StdMutex<Vec<InteractionResponse>>,
    updates: StdMutex<Vec<InteractionResponse>>,
    edits: StdMutex<Vec<(InteractionMessageReceipt, InteractionResponse)>>,
    autocompletes: StdMutex<Vec<Vec<CommandOptionChoice>>>,
    calls: StdMutex<Vec<&'static str>>,
}

impl TestResponder {
    fn new(fail_update: bool) -> Self {
        Self {
            fail_update: AtomicBool::new(fail_update),
            fail_receipt: AtomicBool::new(false),
            defers: StdMutex::new(Vec::new()),
            responses: StdMutex::new(Vec::new()),
            followups: StdMutex::new(Vec::new()),
            updates: StdMutex::new(Vec::new()),
            edits: StdMutex::new(Vec::new()),
            autocompletes: StdMutex::new(Vec::new()),
            calls: StdMutex::new(Vec::new()),
        }
    }

    fn first_button_id(&self) -> String {
        self.followups
            .lock()
            .expect("followups")
            .first()
            .and_then(|response| response.components.first())
            .and_then(|row| row.buttons.first())
            .map(|button| button.custom_id.clone())
            .expect("confirmation button")
    }

    fn button_id_containing(&self, needle: &str) -> String {
        self.followups
            .lock()
            .expect("followups")
            .first()
            .and_then(|response| {
                response
                    .components
                    .iter()
                    .flat_map(|row| row.buttons.iter())
                    .find(|button| button.custom_id.contains(needle))
            })
            .map(|button| button.custom_id.clone())
            .expect("matching button")
    }

    fn call_log(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls").clone()
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().expect("calls").push(call);
    }

    fn fail_receipt(&self) {
        self.fail_receipt.store(true, Ordering::Relaxed);
    }
}

#[async_trait]
impl InteractionResponder for TestResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.record("respond");
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, _ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.record("defer");
        self.defers.lock().expect("defers").push(_ephemeral);
        Ok(())
    }

    async fn autocomplete(
        &self,
        choices: Vec<CommandOptionChoice>,
    ) -> Result<(), InteractionResponseError> {
        self.autocompletes
            .lock()
            .expect("autocompletes")
            .push(choices);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.record("followup");
        self.followups.lock().expect("followups").push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        if self.fail_receipt.load(Ordering::Relaxed) {
            return Err(InteractionResponseError::new("simulated follow-up failure"));
        }
        self.record("followup");
        self.followups.lock().expect("followups").push(response);
        Ok(Some(InteractionMessageReceipt {
            message_id: 1,
            channel_id: CHANNEL,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }))
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        if self.fail_update.load(Ordering::Relaxed) {
            return Err(InteractionResponseError::new("simulated edit failure"));
        }
        self.record("update");
        self.updates.lock().expect("updates").push(response);
        Ok(())
    }

    async fn edit_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.edits.lock().expect("edits").push((receipt, response));
        Ok(())
    }
}

struct ParentTransport {
    parent: StdMutex<Option<u64>>,
}

impl ParentTransport {
    fn new(parent: Option<u64>) -> Self {
        Self {
            parent: StdMutex::new(parent),
        }
    }
}

#[async_trait]
impl DiscordTransport for ParentTransport {
    async fn fetch_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }

    async fn send_message(
        &self,
        channel_id: u64,
        _message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id: 1,
            jump_url: String::new(),
        })
    }

    async fn edit_message(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn delete_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(1)
    }

    async fn pin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn archive_thread(
        &self,
        _thread_id: u64,
        _name: &str,
        _locked: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
        _user_id: u64,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn clear_reaction(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _emoji: &DiscordEmoji,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn unpin_message(&self, _channel_id: u64, _message_id: u64) -> Result<(), String> {
        Ok(())
    }

    async fn send_direct_message(
        &self,
        _user_id: u64,
        _message: DiscordMessage,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn guild_member(
        &self,
        _guild_id: u64,
        _user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        Ok(None)
    }

    async fn channel_parent_id(
        &self,
        _guild_id: u64,
        _channel_id: u64,
    ) -> Result<Option<u64>, String> {
        Ok(*self.parent.lock().expect("parent"))
    }
}

#[derive(Clone, Copy)]
struct FixedClock(i64);

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

fn config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("provider-test-token".to_owned()),
        "PET_CHANNEL_ID" => Some(CHANNEL.to_string()),
        _ => None,
    })
    .expect("provider test config")
}

#[tokio::test]
async fn brawl_channel_gate_accepts_channel_and_parent_thread_only() {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical schema");
    let discord = Arc::new(ParentTransport::new(Some(CHANNEL)));
    let reminders = ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
    let provider = PetRegistrationProvider::new(
        database.path(),
        &config(),
        discord.clone(),
        reminders.hooks(),
        None,
    );
    assert!(
        provider
            .handler
            .brawl_channel_allowed(GUILD as i64, CHANNEL as i64)
            .await
    );
    assert!(
        provider
            .handler
            .brawl_channel_allowed(GUILD as i64, 77_004)
            .await
    );
    *discord.parent.lock().expect("parent") = Some(88_888);
    assert!(
        !provider
            .handler
            .brawl_channel_allowed(GUILD as i64, 77_004)
            .await
    );
}

#[tokio::test]
async fn test_failed_challenge_delivery_voids_and_refunds() {
    let (database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let starting_balance = PlayerRepository::new(database.path())
        .get_by_id(OWNER as i64, Some(GUILD as i64))
        .expect("read starting challenger")
        .expect("starting challenger player")
        .jopacoin_balance;
    let responder = Arc::new(TestResponder::new(false));
    responder.fail_receipt();
    let result = provider
        .handler
        .handle(brawl_command(RECIPIENT, 20), responder)
        .await;
    assert!(
        result.is_err(),
        "the failed first delivery must be surfaced"
    );

    let connection = rusqlite::Connection::open(database.path()).expect("open brawl db");
    let (status, wager): (String, i64) = connection
        .query_row(
            "SELECT status,wager FROM pet_brawls ORDER BY brawl_id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("created challenge row");
    assert_eq!(status, "void");
    assert_eq!(wager, 20);
    let challenger = PlayerRepository::new(database.path())
        .get_by_id(OWNER as i64, Some(GUILD as i64))
        .expect("read challenger")
        .expect("challenger player");
    assert_eq!(challenger.jopacoin_balance, starting_balance);
}

#[tokio::test]
async fn challenge_click_failure_uses_retained_interaction_followup_receipt() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    let decline = initial.first_button_id().replace(":accept", ":decline");
    let click = Arc::new(TestResponder::new(true));
    provider
        .handler
        .handle(component_as(decline, RECIPIENT), click.clone())
        .await
        .expect("retained challenge edit");
    assert!(click.updates.lock().expect("click updates").is_empty());
    let edits = initial.edits.lock().expect("initial edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].0.delivery,
        InteractionMessageDelivery::InteractionFollowup
    );
    assert_preserves_existing_attachments(&edits[0].1);
}

#[tokio::test]
async fn live_pet_brawl_decline_edit_preserves_existing_versus_image() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    assert_eq!(
        initial.followups.lock().expect("challenge followup")[0]
            .attachments
            .len(),
        1,
        "challenge starts with the versus image"
    );

    let decline = initial.first_button_id().replace(":accept", ":decline");
    let click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component_as(decline, RECIPIENT), click.clone())
        .await
        .expect("decline challenge");
    let updates = click.updates.lock().expect("decline updates");
    assert_eq!(updates.len(), 1);
    assert_preserves_existing_attachments(&updates[0]);
}

#[tokio::test]
async fn live_pet_brawl_withdraw_edit_preserves_existing_versus_image() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    assert_eq!(
        initial.followups.lock().expect("challenge followup")[0]
            .attachments
            .len(),
        1,
        "challenge starts with the versus image"
    );

    let withdraw = initial.first_button_id().replace(":accept", ":withdraw");
    let click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component_as(withdraw, OWNER), click.clone())
        .await
        .expect("withdraw challenge");
    let updates = click.updates.lock().expect("withdraw updates");
    assert_eq!(updates.len(), 1);
    assert_preserves_existing_attachments(&updates[0]);
}

#[tokio::test(start_paused = true)]
async fn challenge_timeout_without_raw_id_uses_retained_interaction_followup_receipt() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    assert_eq!(
        initial.followups.lock().expect("challenge followup")[0]
            .attachments
            .len(),
        1,
        "challenge starts with the versus image"
    );
    let challenge_id = brawl_id_from_button(&initial.first_button_id());
    provider
        .handler
        .state
        .challenges
        .lock()
        .expect("challenges")
        .get_mut(&challenge_id)
        .expect("challenge state")
        .message_id = None;
    PetInteractionHandler::expire_challenge_once(Arc::clone(&provider.handler.state), challenge_id)
        .await
        .expect("challenge timeout delivery");
    let edits = initial.edits.lock().expect("initial edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].0.delivery,
        InteractionMessageDelivery::InteractionFollowup
    );
    assert_eq!(
        edits[0].1.embeds[0].title.as_deref(),
        Some("🌾 Challenge expired")
    );
    assert_preserves_existing_attachments(&edits[0].1);
}

#[tokio::test]
async fn battle_click_failure_uses_retained_interaction_followup_receipt() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    assert_eq!(
        initial.followups.lock().expect("challenge followup")[0]
            .attachments
            .len(),
        1,
        "challenge starts with the versus image"
    );
    let accept = initial.first_button_id();
    let accept_click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(
            component_as(accept.clone(), RECIPIENT),
            accept_click.clone(),
        )
        .await
        .expect("battle start");
    {
        let accept_updates = accept_click.updates.lock().expect("accept updates");
        assert_eq!(accept_updates.len(), 1);
        assert_preserves_existing_attachments(&accept_updates[0]);
    }
    let battle_id = initial
        .followups
        .lock()
        .expect("followups")
        .first()
        .map(|response| brawl_id_from_button(&response.components[0].buttons[0].custom_id))
        .expect("challenge id");
    let move_id = provider
        .handler
        .state
        .battle_views
        .lock()
        .expect("battle views")
        .get(&battle_id)
        .expect("battle state")
        .view_model()
        .buttons[0]
        .custom_id
        .clone()
        .expect("move id");
    let click = Arc::new(TestResponder::new(true));
    provider
        .handler
        .handle(component_as(move_id, OWNER), click.clone())
        .await
        .expect("retained battle edit");
    assert!(click.updates.lock().expect("click updates").is_empty());
    assert!(
        click.followups.lock().expect("click followups").is_empty(),
        "a rejected initial callback leaves the interaction unacknowledged, so no follow-up may be attempted"
    );
    let edits = initial.edits.lock().expect("initial edits");
    assert!(edits.iter().any(|(receipt, _)| {
        receipt.delivery == InteractionMessageDelivery::InteractionFollowup
    }));
}

#[tokio::test]
async fn battle_click_confirms_the_locked_in_move_privately() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    let accept = initial.first_button_id();
    let accept_click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(
            component_as(accept.clone(), RECIPIENT),
            accept_click.clone(),
        )
        .await
        .expect("battle start");
    let battle_id = initial
        .followups
        .lock()
        .expect("followups")
        .first()
        .map(|response| brawl_id_from_button(&response.components[0].buttons[0].custom_id))
        .expect("challenge id");
    let button = provider
        .handler
        .state
        .battle_views
        .lock()
        .expect("battle views")
        .get(&battle_id)
        .expect("battle state")
        .view_model()
        .buttons[0]
        .clone();
    let move_id = button.custom_id.clone().expect("move id");
    let click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component_as(move_id, OWNER), click.clone())
        .await
        .expect("battle pick");

    let followups = click.followups.lock().expect("click followups");
    let confirmation = followups
        .first()
        .expect("the picker is told privately which move locked in");
    assert!(
        confirmation.content.contains("You picked"),
        "unexpected confirmation: {:?}",
        confirmation.content
    );
    assert!(
        confirmation.ephemeral,
        "a pick confirmation is private: picks are secret until the round resolves"
    );
    assert_eq!(
        click.call_log(),
        vec!["update", "followup"],
        "the board edit is this interaction's one initial callback, so the private confirmation follows it"
    );
}

#[tokio::test]
async fn battle_round_transition_accepts_next_round_clicks() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    let accept = initial.first_button_id();
    provider
        .handler
        .handle(
            component_as(accept.clone(), RECIPIENT),
            Arc::new(TestResponder::new(false)),
        )
        .await
        .expect("battle start");
    let battle_id = brawl_id_from_button(&accept);
    let first_move = provider
        .handler
        .state
        .battle_views
        .lock()
        .expect("battle views")
        .get(&battle_id)
        .expect("battle state")
        .view_model()
        .buttons[0]
        .custom_id
        .clone()
        .expect("first move");
    provider
        .handler
        .handle(
            component_as(first_move.clone(), OWNER),
            Arc::new(TestResponder::new(false)),
        )
        .await
        .expect("challenger first move");
    provider
        .handler
        .handle(
            component_as(first_move, RECIPIENT),
            Arc::new(TestResponder::new(false)),
        )
        .await
        .expect("recipient first move");
    let second_move = provider
        .handler
        .state
        .battle_views
        .lock()
        .expect("next-round battle views")
        .get(&battle_id)
        .expect("next-round battle state")
        .view_model()
        .buttons[0]
        .custom_id
        .clone()
        .expect("next-round move");
    assert!(second_move.contains(":1:"));
    provider
        .handler
        .handle(
            component_as(second_move, OWNER),
            Arc::new(TestResponder::new(false)),
        )
        .await
        .expect("challenger next-round move");
}

#[tokio::test(start_paused = true)]
async fn terminal_battle_timeout_uses_retained_interaction_followup_receipt() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    let accept = initial.first_button_id();
    provider
        .handler
        .handle(
            component_as(accept, RECIPIENT),
            Arc::new(TestResponder::new(false)),
        )
        .await
        .expect("battle start");
    let battle_id = brawl_id_from_button(&initial.first_button_id());
    provider
        .handler
        .state
        .battle_views
        .lock()
        .expect("battle views")
        .get_mut(&battle_id)
        .expect("battle state")
        .message_id = None;
    assert!(
        !PetInteractionHandler::advance_battle_timeout_once(
            Arc::clone(&provider.handler.state),
            battle_id,
        )
        .await
        .expect("first battle timeout delivery")
    );
    assert!(
        PetInteractionHandler::advance_battle_timeout_once(
            Arc::clone(&provider.handler.state),
            battle_id,
        )
        .await
        .expect("terminal battle timeout delivery")
    );
    let edits = initial.edits.lock().expect("initial edits");
    assert!(
        edits.len() >= 2,
        "first and terminal timeouts edit the receipt"
    );
    assert!(edits.iter().all(|(receipt, _)| {
        receipt.delivery == InteractionMessageDelivery::InteractionFollowup
    }));
}

fn fixture(adult: bool) -> (NamedTempFile, PetRegistrationProvider, i64) {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(
            i64::try_from(OWNER).expect("owner id"),
            "Provider Test Owner",
            Some(i64::try_from(GUILD).expect("guild id")),
        ))
        .expect("register owner");
    players
        .update_balance(
            i64::try_from(OWNER).expect("owner id"),
            Some(i64::try_from(GUILD).expect("guild id")),
            10_000,
        )
        .expect("fund owner");

    let now = SystemPetClock.now();
    let adopted_at = if adult {
        now - EGG_HATCH_SECONDS - ADULT_AGE_SECONDS - 10
    } else {
        now - EGG_HATCH_SECONDS - 10
    };
    let mut service = SqlitePetCommandService::new(
        database.path(),
        SeededPetRandom::new(11),
        FixedClock(adopted_at),
        20,
    );
    let adopted = match service.adopt(
        i64::try_from(OWNER).expect("owner id"),
        Some(i64::try_from(GUILD).expect("guild id")),
        "Provider Test Pet",
        "standard",
    ) {
        ServiceResult::Success(outcome) => outcome.pet,
        ServiceResult::Failure { error, .. } => panic!("adopt fixture pet: {error}"),
    };
    drop(service);
    PetRepository::new(database.path())
        .resolve_hatch_species(
            adopted.pet_id,
            Some(i64::try_from(GUILD).expect("guild id")),
            "common_cama",
            now,
        )
        .expect("resolve fixture pet");
    rusqlite::Connection::open(database.path())
        .expect("open fixture")
        .execute(
            "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1 WHERE pet_id=?2",
            rusqlite::params![now, adopted.pet_id],
        )
        .expect("refresh fixture hunger");

    let discord = Arc::new(SerenityDiscordTransport::new());
    let reminder_provider =
        ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
    let provider = PetRegistrationProvider::new_without_ai(
        database.path(),
        &config(),
        discord,
        reminder_provider.hooks(),
    );
    (database, provider, adopted.pet_id)
}

fn empty_fixture() -> (NamedTempFile, PetRegistrationProvider) {
    let database = NamedTempFile::new().expect("temporary empty database");
    initialize_or_migrate(database.path()).expect("canonical schema");
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(
            i64::try_from(OWNER).expect("owner id"),
            "Provider Test Owner",
            Some(i64::try_from(GUILD).expect("guild id")),
        ))
        .expect("register empty fixture owner");
    players
        .update_balance(
            i64::try_from(OWNER).expect("owner id"),
            Some(i64::try_from(GUILD).expect("guild id")),
            10_000,
        )
        .expect("fund empty fixture owner");
    let discord = Arc::new(SerenityDiscordTransport::new());
    let reminder_provider =
        ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
    let provider = PetRegistrationProvider::new_without_ai(
        database.path(),
        &config(),
        discord,
        reminder_provider.hooks(),
    );
    (database, provider)
}

fn brawl_fixture() -> (NamedTempFile, PetRegistrationProvider, i64, i64) {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical schema");
    let players = PlayerRepository::new(database.path());
    for (user_id, name) in [
        (OWNER, "Provider Test Challenger"),
        (RECIPIENT, "Provider Test Recipient"),
    ] {
        players
            .add(&NewPlayer::new(
                i64::try_from(user_id).expect("user id"),
                name,
                Some(i64::try_from(GUILD).expect("guild id")),
            ))
            .expect("register brawl player");
        players
            .update_balance(
                i64::try_from(user_id).expect("user id"),
                Some(i64::try_from(GUILD).expect("guild id")),
                10_000,
            )
            .expect("fund brawl player");
    }
    let now = SystemPetClock.now();
    let adopted_at = now - EGG_HATCH_SECONDS - ADULT_AGE_SECONDS - 10;
    let mut pet_ids = Vec::new();
    for (user_id, name, seed) in [
        (OWNER, "Provider Test Challenger Pet", 11_u64),
        (RECIPIENT, "Provider Test Recipient Pet", 13_u64),
    ] {
        let mut service = SqlitePetCommandService::new(
            database.path(),
            SeededPetRandom::new(seed),
            FixedClock(adopted_at),
            20,
        );
        let adopted = match service.adopt(
            i64::try_from(user_id).expect("user id"),
            Some(i64::try_from(GUILD).expect("guild id")),
            name,
            "standard",
        ) {
            ServiceResult::Success(outcome) => outcome.pet,
            ServiceResult::Failure { error, .. } => panic!("adopt brawl fixture pet: {error}"),
        };
        drop(service);
        PetRepository::new(database.path())
            .resolve_hatch_species(
                adopted.pet_id,
                Some(i64::try_from(GUILD).expect("guild id")),
                "common_cama",
                now,
            )
            .expect("resolve brawl fixture pet");
        rusqlite::Connection::open(database.path())
                .expect("open brawl fixture")
                .execute(
                    "UPDATE pets SET last_fed_at=?1,hunger_at_last_fed=100,dig_work_at=?1 WHERE pet_id=?2",
                    rusqlite::params![now, adopted.pet_id],
                )
                .expect("refresh brawl fixture hunger");
        pet_ids.push(adopted.pet_id);
    }
    let discord = Arc::new(SerenityDiscordTransport::new());
    let reminder_provider =
        ReminderRegistrationProvider::new(database.path(), &config(), discord.clone());
    let provider = PetRegistrationProvider::new_without_ai(
        database.path(),
        &config(),
        discord,
        reminder_provider.hooks(),
    );
    (database, provider, pet_ids[0], pet_ids[1])
}

fn altar_command() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "pet".to_owned(),
        user_id: OWNER,
        user_display_name: "Provider Test Owner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "altar".to_owned(),
            value: InteractionValue::Subcommand(vec![InteractionOption {
                name: "name".to_owned(),
                value: InteractionValue::String("Rebirth".to_owned()),
            }]),
        }],
    }
}

fn eat_command() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "pet".to_owned(),
        user_id: OWNER,
        user_display_name: "Provider Test Owner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "eat".to_owned(),
            value: InteractionValue::Subcommand(Vec::new()),
        }],
    }
}

fn status_command() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 3,
        name: "pet".to_owned(),
        user_id: OWNER,
        user_display_name: "Provider Test Owner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "status".to_owned(),
            value: InteractionValue::Subcommand(Vec::new()),
        }],
    }
}

fn brawl_command(target_id: u64, wager: i64) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 4,
        name: "pet".to_owned(),
        user_id: OWNER,
        user_display_name: "Provider Test Challenger".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "brawl".to_owned(),
            value: InteractionValue::Subcommand(vec![
                InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: target_id,
                        display_name: Some("Provider Test Recipient".to_owned()),
                        is_bot: Some(false),
                    },
                },
                InteractionOption {
                    name: "wager".to_owned(),
                    value: InteractionValue::Integer(wager),
                },
            ]),
        }],
    }
}

fn autocomplete_request(current: &str) -> InteractionRequest {
    InteractionRequest::Autocomplete {
        interaction_id: 5,
        name: "pet".to_owned(),
        user_id: OWNER,
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        focused_option: "wear".to_owned(),
        focused_value: current.to_owned(),
        options: Vec::new(),
    }
}

fn leaf_request(subcommand: &str, options: Vec<InteractionOption>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 6,
        name: "pet".to_owned(),
        user_id: OWNER,
        user_display_name: "Provider Test Challenger".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn component(custom_id: String) -> InteractionRequest {
    component_as(custom_id, OWNER)
}

fn component_as(custom_id: String, user_id: u64) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id,
        user_id,
        user_display_name: "Provider Test Owner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        values: Vec::new(),
    }
}

fn assert_preserves_existing_attachments(response: &InteractionResponse) {
    assert_eq!(
        response.attachment_policy,
        InteractionAttachmentPolicy::Preserve,
        "pet-brawl edits must omit attachments when no replacement art is supplied"
    );
    assert!(response.attachments.is_empty());
}

fn brawl_id_from_button(custom_id: &str) -> i64 {
    custom_id
        .split(':')
        .nth(2)
        .expect("brawl id segment")
        .parse()
        .expect("numeric brawl id")
}

fn death_announcement(path: &Path, pet_id: i64) -> Option<i64> {
    PetRepository::new(path)
        .get_pet_by_id(pet_id, Some(i64::try_from(GUILD).expect("guild id")))
        .expect("read pet")
        .expect("pet row")
        .death_announced_at
}

#[test]
fn command_schema_matches_python_pet_group() {
    let options = pet_options();
    assert_eq!(
        options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        vec![
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
        ]
    );
    let descriptions = options
        .iter()
        .map(|option| (option.name.clone(), option.description.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        descriptions,
        BTreeMap::from([
            ("adopt".to_owned(), "Adopt a mysterious cama egg".to_owned()),
            (
                "status".to_owned(),
                "Check on your cama (art, fullness, mood)".to_owned(),
            ),
            (
                "train".to_owned(),
                "Cash in up to three banked solo training sessions".to_owned(),
            ),
            (
                "feed".to_owned(),
                "Feed your cama from your supplies".to_owned()
            ),
            ("shop".to_owned(), "Browse cama food and treats".to_owned()),
            ("buy".to_owned(), "Buy cama supplies".to_owned()),
            ("rename".to_owned(), "Rename your cama (10 JC)".to_owned()),
            (
                "trinket".to_owned(),
                format!("Roll a Mystery Trinket ({TRINKET_COST} JC) or wear one you own"),
            ),
            (
                "brawl".to_owned(),
                "Challenge someone to a pet brawl, optionally for up to 100 JC".to_owned(),
            ),
            (
                "altar".to_owned(),
                "Sacrifice your cama for a better egg (dark, effective)".to_owned(),
            ),
            (
                "eat".to_owned(),
                "Make an irreversible choice about your adult cama".to_owned(),
            ),
            (
                "graveyard".to_owned(),
                "Visit the cama memorial garden".to_owned(),
            ),
            (
                "leaderboard".to_owned(),
                "The oldest living camas".to_owned()
            ),
        ])
    );

    let adopt = &options[0];
    assert_eq!(
        adopt
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        vec!["name", "egg"]
    );
    assert!(adopt.options[0].required);
    assert!(!adopt.options[1].required);
    assert_eq!(
        adopt.options[1].description,
        format!("Gilded Egg: +{GILDED_EGG_PREMIUM} JC, no commons in the pool")
    );
    assert_eq!(
        adopt.options[1].choices,
        vec![
            CommandOptionChoice::String {
                name: "Standard Egg".to_owned(),
                value: "standard".to_owned(),
            },
            CommandOptionChoice::String {
                name: format!("Gilded Egg (+{GILDED_EGG_PREMIUM} JC, uncommon or better)"),
                value: "gilded".to_owned(),
            },
        ]
    );

    let status = &options[1];
    assert_eq!(
        status
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        vec!["user", "public"]
    );
    assert!(!status.options[0].required && !status.options[1].required);
    let feed = &options[3];
    assert!(feed.options[0].required);
    assert_eq!(
        feed.options[0].choices,
        FOOD_ITEMS
            .iter()
            .map(|food| CommandOptionChoice::String {
                name: format!(
                    "{} ({} JC, +{} fullness)",
                    food.display_name, food.cost, food.restore
                ),
                value: food.item_id.to_owned(),
            })
            .collect::<Vec<_>>()
    );
    let buy = &options[5];
    assert_eq!(buy.options[1].min_integer, Some(1));
    assert_eq!(buy.options[1].max_integer, Some(MAX_BUY_QTY));
    assert_eq!(
        buy.options[0].choices.last(),
        Some(&CommandOptionChoice::String {
            name: format!(
                "{} ({} JC, pampers instantly)",
                SALT_LICK.display_name, SALT_LICK.cost
            ),
            value: SALT_LICK.item_id.to_owned(),
        })
    );
    assert!(options[6].options[0].required);
    assert!(options[7].options[0].autocomplete);
    assert!(options[8].options[0].required);
    assert_eq!(options[8].options[1].min_integer, Some(0));
    assert_eq!(options[8].options[1].max_integer, Some(100));
    assert!(options[9].options[0].required);
    assert!(!options[11].options[0].required);
}

#[test]
fn status_timeout_disables_controls_and_ignores_stale_generation() {
    let response =
        InteractionResponse::message("status").action_rows(vec![InteractionActionRow::buttons(
            vec![InteractionButton::new("pet:status:1:feed:grain", "Feed")],
        )]);
    let disabled = disabled_status_response(response.clone());
    assert_eq!(disabled.content, response.content);
    assert!(disabled.components[0].buttons[0].disabled);

    let now = Instant::now();
    let responder: Arc<dyn InteractionResponder> = Arc::new(TestResponder::new(false));
    let mut view = StatusViewState {
        owner_id: 1,
        guild_id: 2,
        receipt: None,
        responder,
        response,
        public: false,
        generation: 0,
        expires_at: now + STATUS_TIMEOUT,
    };
    assert!(!status_timeout_is_current(&view, 0, now));
    view.generation = 1;
    view.expires_at = now;
    assert!(!status_timeout_is_current(&view, 0, now));
    assert!(status_timeout_is_current(&view, 1, now));
}

#[tokio::test(start_paused = true)]
async fn status_timeout_edits_the_interaction_followup_receipt() {
    let (_database, provider, _pet_id) = fixture(false);
    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(status_command(), responder.clone())
        .await
        .expect("status response");
    assert_eq!(
        provider
            .handler
            .state
            .status_views
            .lock()
            .expect("status views")
            .len(),
        1
    );
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::time::advance(STATUS_TIMEOUT + Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let edits = responder.edits.lock().expect("edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].0.delivery,
        InteractionMessageDelivery::InteractionFollowup
    );
    assert!(
        edits[0]
            .1
            .components
            .iter()
            .flat_map(|row| row.buttons.iter())
            .all(|button| button.disabled)
    );
}

#[tokio::test]
async fn status_refresh_edits_the_retained_interaction_followup() {
    let (_database, provider, _pet_id) = fixture(false);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(status_command(), initial.clone())
        .await
        .expect("status response");
    let click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component(initial.button_id_containing(":buy:")), click)
        .await
        .expect("status refresh");
    let edits = initial.edits.lock().expect("edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].0.delivery,
        InteractionMessageDelivery::InteractionFollowup
    );
    assert!(!edits[0].1.components.is_empty());
}

#[tokio::test]
async fn status_click_after_process_restart_is_expired_and_ephemeral() {
    let (_database, provider, _pet_id) = fixture(false);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(status_command(), initial.clone())
        .await
        .expect("status response");
    let button = initial.button_id_containing(":buy:");
    provider
        .handler
        .state
        .status_views
        .lock()
        .expect("status views")
        .clear();
    let restarted = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component(button), restarted.clone())
        .await
        .expect("expired status click");
    let followups = restarted.followups.lock().expect("restart followups");
    assert_eq!(followups.len(), 0);
    assert!(
        restarted
            .updates
            .lock()
            .expect("restart updates")
            .is_empty()
    );
    let responses = restarted.responses.lock().expect("restart responses");
    assert_eq!(responses.len(), 1);
    assert!(responses[0].ephemeral);
    assert!(responses[0].content.contains("expired"));
}

#[tokio::test(start_paused = true)]
async fn altar_confirmation_timeout_uses_the_cold_altar_embed() {
    let (_database, provider, _pet_id) = fixture(false);
    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(altar_command(), responder.clone())
        .await
        .expect("altar preview");
    tokio::task::yield_now().await;
    tokio::time::advance(CONFIRMATION_TIMEOUT).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let edits = responder.edits.lock().expect("edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].1.embeds[0].title.as_deref(),
        Some("🕯️ The altar goes cold")
    );
    assert!(edits[0].1.components.is_empty());
}

#[tokio::test(start_paused = true)]
async fn eat_confirmation_timeout_uses_the_hunger_passes_embed() {
    let (_database, provider, _pet_id) = fixture(true);
    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(eat_command(), responder.clone())
        .await
        .expect("eat preview");
    tokio::task::yield_now().await;
    tokio::time::advance(CONFIRMATION_TIMEOUT).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let edits = responder.edits.lock().expect("edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].1.embeds[0].title.as_deref(),
        Some("The hunger passes")
    );
    assert!(edits[0].1.components.is_empty());
}

#[tokio::test]
async fn test_failed_edit_falls_back_and_still_marks() {
    let (database, provider, pet_id) = fixture(false);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(altar_command(), initial.clone())
        .await
        .expect("altar preview");
    let confirm_id = initial.first_button_id();
    let fallback = Arc::new(TestResponder::new(true));
    provider
        .handler
        .handle(component(confirm_id), fallback.clone())
        .await
        .expect("altar fallback delivery");
    assert!(fallback.updates.lock().expect("updates").is_empty());
    assert_eq!(fallback.followups.lock().expect("followups").len(), 1);
    assert!(death_announcement(database.path(), pet_id).is_some());
}

#[tokio::test]
async fn test_confirm_performs_the_rite() {
    let (database, provider, pet_id) = fixture(false);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(altar_command(), initial.clone())
        .await
        .expect("altar preview");
    let confirmation = initial.first_button_id();
    let success = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component(confirmation), success)
        .await
        .expect("altar delivery");
    assert!(death_announcement(database.path(), pet_id).is_some());
    assert!(
        PetRepository::new(database.path())
            .get_unannounced_deaths(20)
            .expect("read unannounced deaths")
            .is_empty()
    );
}

#[tokio::test]
async fn test_cancel_keeps_the_pet() {
    let (database, provider, pet_id) = fixture(false);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(altar_command(), initial.clone())
        .await
        .expect("altar preview");
    let cancel = initial.first_button_id().replace(":confirm", ":cancel");
    provider
        .handler
        .handle(component(cancel), Arc::new(TestResponder::new(false)))
        .await
        .expect("altar cancellation");
    let pet = PetRepository::new(database.path())
        .get_pet_by_id(pet_id, Some(i64::try_from(GUILD).expect("guild id")))
        .expect("read pet")
        .expect("pet row");
    assert!(pet.died_at.is_none());
    assert!(pet.death_announced_at.is_none());
}

#[tokio::test]
async fn eat_failed_edit_falls_back_and_marks_death_for_restart_sweep() {
    let (database, provider, pet_id) = fixture(true);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(eat_command(), initial.clone())
        .await
        .expect("eat preview");
    let confirm = initial.first_button_id();
    let fallback = Arc::new(TestResponder::new(true));
    provider
        .handler
        .handle(component(confirm), fallback.clone())
        .await
        .expect("eat fallback delivery");
    assert!(fallback.updates.lock().expect("updates").is_empty());
    assert_eq!(fallback.followups.lock().expect("followups").len(), 1);
    assert!(death_announcement(database.path(), pet_id).is_some());
    assert!(!shared_direct_death_delivery_active(
        database.path(),
        pet_id
    ));
}

#[test]
fn direct_death_delivery_suppression_is_nested_and_released() {
    let database = tempfile::NamedTempFile::new().expect("temporary guard path");
    let pet_id = 9_999_991;
    assert!(!shared_direct_death_delivery_active(
        database.path(),
        pet_id
    ));
    {
        let _guard = DirectDeathDeliveryGuard::new(database.path(), pet_id);
        assert!(shared_direct_death_delivery_active(database.path(), pet_id));
        {
            let _nested_guard = DirectDeathDeliveryGuard::new(database.path(), pet_id);
            assert!(shared_direct_death_delivery_active(database.path(), pet_id));
        }
        assert!(shared_direct_death_delivery_active(database.path(), pet_id));
    }
    assert!(!shared_direct_death_delivery_active(
        database.path(),
        pet_id
    ));
}

#[tokio::test]
async fn test_sweep_tombstone_names_the_altar() {
    let (_database, provider, _pet_id) = fixture(false);
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(altar_command(), initial.clone())
        .await
        .expect("altar preview");
    let success = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component(initial.first_button_id()), success.clone())
        .await
        .expect("altar delivery");
    let update = success
        .updates
        .lock()
        .expect("updates")
        .first()
        .cloned()
        .expect("altar tombstone update");
    assert_eq!(
        update.embeds[0].title.as_deref(),
        Some("🩸 Provider Test Pet was given to the altar")
    );
    assert!(
        update.embeds[0]
            .image_url
            .as_deref()
            .is_some_and(|url| url.starts_with("attachment://"))
    );
    assert_eq!(update.attachments.len(), 1);
}

#[tokio::test]
async fn live_adopt_dispatch_creates_the_egg_and_media() {
    let (database, provider) = empty_fixture();
    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(
            leaf_request(
                "adopt",
                vec![InteractionOption {
                    name: "name".to_owned(),
                    value: InteractionValue::String("Fresh Egg".to_owned()),
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("adopt dispatch");
    assert_eq!(
        responder.defers.lock().expect("adopt defers").as_slice(),
        &[false]
    );
    let followups = responder.followups.lock().expect("adopt followups");
    assert_eq!(followups.len(), 1);
    assert_eq!(followups[0].attachments.len(), 1);
    assert_eq!(
        followups[0].embeds[0].image_url.as_deref(),
        Some(format!(
            "attachment://{}",
            followups[0].attachments[0].filename
        ))
        .as_deref()
    );
    assert!(
        followups[0].embeds[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains("Fresh Egg"))
    );
    drop(followups);
    let connection = rusqlite::Connection::open(database.path()).expect("open adopt db");
    let (name, species): (String, String) = connection
        .query_row(
            "SELECT name,species FROM pets WHERE discord_id=?1 ORDER BY pet_id DESC LIMIT 1",
            [i64::try_from(OWNER).expect("owner id")],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("adopted egg row");
    assert_eq!(name, "Fresh Egg");
    assert_eq!(species, UNHATCHED_SPECIES);
}

#[tokio::test]
async fn live_trinket_roll_persists_equipment_and_autocomplete() {
    let (database, provider, pet_id) = fixture(true);
    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(leaf_request("trinket", Vec::new()), responder.clone())
        .await
        .expect("trinket dispatch");
    assert_eq!(
        responder.defers.lock().expect("trinket defers").as_slice(),
        &[true]
    );
    let content = responder
        .followups
        .lock()
        .expect("trinket followups")
        .first()
        .map(|response| response.content.clone())
        .expect("trinket response");
    assert!(content.contains("Equipped!") || content.contains("duplicate"));
    let connection = rusqlite::Connection::open(database.path()).expect("open trinket db");
    let accessory: Option<String> = connection
        .query_row(
            "SELECT accessory FROM pets WHERE pet_id=?1",
            [pet_id],
            |row| row.get(0),
        )
        .expect("equipped accessory row");
    assert!(accessory.is_some());
    let autocomplete = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(autocomplete_request(""), autocomplete.clone())
        .await
        .expect("trinket autocomplete");
    assert_eq!(
        autocomplete
            .autocompletes
            .lock()
            .expect("trinket choices")
            .len(),
        1
    );
}

#[tokio::test]
async fn live_status_renders_evolution_pampered_trinket_brawl_projection() {
    let (database, provider, pet_id, recipient_pet_id) = brawl_fixture();
    let now = SystemPetClock.now();
    let connection = rusqlite::Connection::open(database.path()).expect("open status db");
    connection
        .execute(
            "UPDATE pets
                 SET pampered_until=?1,
                     accessory='top_hat',
                     evolution_calling='oracle',
                     evolution_primary='wisdom',
                     evolution_secondary='fellowship',
                     evolved_at=?2
                 WHERE pet_id=?3",
            rusqlite::params![now + 3_600, now - 10, pet_id],
        )
        .expect("seed evolved status");
    connection
        .execute(
            "INSERT INTO pet_brawls (
                    guild_id,channel_id,challenger_id,recipient_id,
                    challenger_pet_id,recipient_pet_id,status,created_at,expires_at,
                    resolved_at,winner_id,winner_pet_id,loser_pet_id,rounds
                 ) VALUES (?1,?2,?3,?4,?5,?6,'done',?7,?7,?7,?3,?5,?6,1)",
            rusqlite::params![
                i64::try_from(GUILD).expect("guild"),
                i64::try_from(CHANNEL).expect("channel"),
                i64::try_from(OWNER).expect("owner"),
                i64::try_from(RECIPIENT).expect("recipient"),
                pet_id,
                recipient_pet_id,
                now - 100,
            ],
        )
        .expect("seed brawl record");

    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(status_command(), responder.clone())
        .await
        .expect("status dispatch");
    let followups = responder.followups.lock().expect("status followups");
    let embed = followups[0].embeds.first().expect("status embed");
    assert!(
        embed
            .title
            .as_deref()
            .is_some_and(|title| title.contains("Oracle"))
    );
    assert!(
        embed
            .description
            .as_deref()
            .is_some_and(|description| description.starts_with('_'))
    );
    let field = |name: &str| {
        embed
            .fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
    };
    assert!(field("Mood").is_some_and(|value| value.contains("pampered")));
    assert!(field("Calling").is_some_and(|value| value.contains("Paths: Wisdom + Fellowship")));
    assert_eq!(field("Trinket"), Some("🎩 Top Hat"));
    assert_eq!(field("⚔️ Brawls"), Some("1W · 0L"));
    assert!(field("🏋️ Training").is_some_and(|value| value.contains("Solo:")));
}

#[tokio::test]
async fn live_graveyard_and_leaderboard_render_python_projection_fields() {
    let (database, provider, pet_id, recipient_pet_id) = brawl_fixture();
    let now = SystemPetClock.now();
    let connection = rusqlite::Connection::open(database.path()).expect("open presentation db");
    connection
        .execute(
            "UPDATE pets
                 SET evolution_calling='oracle', evolution_primary='wisdom',
                     evolution_secondary='fellowship', evolved_at=?1
                 WHERE pet_id=?2",
            rusqlite::params![now - 10, pet_id],
        )
        .expect("seed calling");
    connection
        .execute(
            "INSERT INTO pet_brawls (
                    guild_id,channel_id,challenger_id,recipient_id,
                    challenger_pet_id,recipient_pet_id,status,created_at,expires_at,
                    resolved_at,winner_id,winner_pet_id,loser_pet_id,rounds
                 ) VALUES (?1,?2,?3,?4,?5,?6,'done',?7,?7,?7,?4,?6,?5,1)",
            rusqlite::params![
                i64::try_from(GUILD).expect("guild"),
                i64::try_from(CHANNEL).expect("channel"),
                i64::try_from(OWNER).expect("owner"),
                i64::try_from(RECIPIENT).expect("recipient"),
                pet_id,
                recipient_pet_id,
                now - 100,
            ],
        )
        .expect("seed leaderboard record");

    let altar = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(altar_command(), altar.clone())
        .await
        .expect("graveyard altar preview");
    provider
        .handler
        .handle(
            component(altar.first_button_id()),
            Arc::new(TestResponder::new(false)),
        )
        .await
        .expect("graveyard sacrifice");

    let graveyard = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(leaf_request("graveyard", Vec::new()), graveyard.clone())
        .await
        .expect("graveyard dispatch");
    let graveyard_embed =
        graveyard.followups.lock().expect("graveyard followup")[0].embeds[0].clone();
    assert!(
        graveyard_embed
            .description
            .as_deref()
            .is_some_and(|description| {
                description.contains("Oracle") && description.contains("<t:")
            })
    );
    assert!(
        graveyard_embed
            .fields
            .iter()
            .any(|field| field.name.starts_with("📖 Camadex"))
    );
    assert!(
        graveyard_embed
            .fields
            .iter()
            .any(|field| field.name.starts_with("Callings"))
    );

    let leaderboard = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(leaf_request("leaderboard", Vec::new()), leaderboard.clone())
        .await
        .expect("leaderboard dispatch");
    let leaderboard_embed =
        leaderboard.followups.lock().expect("leaderboard followup")[0].embeds[0].clone();
    assert!(
        leaderboard_embed
            .description
            .as_deref()
            .is_some_and(|description| {
                description.contains("🥇") && description.contains("😊")
            })
    );
    assert!(
        leaderboard_embed
            .description
            .as_deref()
            .is_some_and(|description| { description.contains("⚔️ 1W-0L") })
    );
}

#[tokio::test]
async fn status_feed_button_and_footer_reset_on_game_day_rollover() {
    let (database, provider, pet_id) = fixture(true);
    let now = SystemPetClock.now();
    let today =
        cama_domain::game_date::game_date_for_timestamp(now as f64).expect("current game date");
    let connection = rusqlite::Connection::open(database.path()).expect("open rollover db");
    connection
        .execute(
            "UPDATE pets SET feeds_today=4,feed_date='2000-01-01' WHERE pet_id=?1",
            [pet_id],
        )
        .expect("seed stale feed counter");
    connection
        .execute(
            "INSERT INTO pet_supplies (discord_id,guild_id,item_id,qty,updated_at)
                 VALUES (?1,?2,'tango',1,?3)
                 ON CONFLICT(discord_id,guild_id,item_id)
                 DO UPDATE SET qty=excluded.qty,updated_at=excluded.updated_at",
            rusqlite::params![
                i64::try_from(OWNER).expect("owner"),
                i64::try_from(GUILD).expect("guild"),
                now,
            ],
        )
        .expect("seed feed supply");
    let responder = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(status_command(), responder.clone())
        .await
        .expect("rollover status");
    let followup = responder.followups.lock().expect("rollover followup");
    let feed_button = followup[0]
        .components
        .iter()
        .flat_map(|row| row.buttons.iter())
        .find(|button| button.custom_id.ends_with(":feed:tango"))
        .expect("tango feed button");
    assert!(!feed_button.disabled, "stale prior-day count must reset");
    assert_eq!(
        followup[0].embeds[0].footer.as_deref(),
        Some("4/4 feeds left today")
    );
    assert!(!today.is_empty());
}

#[tokio::test]
async fn live_migrated_sqlite_dispatches_all_pet_leaves_and_persistent_paths() {
    let (_database, provider, _challenger_pet, _recipient_pet) = brawl_fixture();
    let leaves = vec![
        (
            "adopt",
            vec![InteractionOption {
                name: "name".to_owned(),
                value: InteractionValue::String("Second Egg".to_owned()),
            }],
            false,
        ),
        ("status", Vec::new(), true),
        ("train", Vec::new(), true),
        ("shop", Vec::new(), true),
        (
            "buy",
            vec![
                InteractionOption {
                    name: "item".to_owned(),
                    value: InteractionValue::String("tango".to_owned()),
                },
                InteractionOption {
                    name: "qty".to_owned(),
                    value: InteractionValue::Integer(1),
                },
            ],
            true,
        ),
        (
            "feed",
            vec![InteractionOption {
                name: "item".to_owned(),
                value: InteractionValue::String("tango".to_owned()),
            }],
            true,
        ),
        (
            "rename",
            vec![InteractionOption {
                name: "name".to_owned(),
                value: InteractionValue::String("Renamed Pet".to_owned()),
            }],
            true,
        ),
        ("trinket", Vec::new(), true),
        ("eat", Vec::new(), false),
        (
            "brawl",
            vec![
                InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: RECIPIENT,
                        display_name: Some("Provider Test Recipient".to_owned()),
                        is_bot: Some(false),
                    },
                },
                InteractionOption {
                    name: "wager".to_owned(),
                    value: InteractionValue::Integer(0),
                },
            ],
            false,
        ),
        (
            "altar",
            vec![InteractionOption {
                name: "name".to_owned(),
                value: InteractionValue::String("Altar Egg".to_owned()),
            }],
            false,
        ),
        ("graveyard", Vec::new(), false),
        ("leaderboard", Vec::new(), false),
    ];
    for (name, options, expected_defer) in leaves {
        let responder = Arc::new(TestResponder::new(false));
        provider
            .handler
            .handle(leaf_request(name, options), responder.clone())
            .await
            .unwrap_or_else(|error| panic!("/pet {name} dispatch: {error}"));
        assert_eq!(
            responder.defers.lock().expect("defer records").as_slice(),
            &[expected_defer],
            "/pet {name} defer visibility"
        );
        assert!(
            !responder.followups.lock().expect("followups").is_empty(),
            "/pet {name} must deliver a response"
        );
        let followups = responder.followups.lock().expect("followups");
        if name == "status" {
            assert!(!followups[0].embeds.is_empty());
            assert!(!followups[0].attachments.is_empty(), "status media");
        }
        if name == "buy" {
            assert!(
                followups
                    .iter()
                    .any(|response| { response.content.contains(JOPACOIN_EMOTE) })
            );
        }
    }

    let autocomplete = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(autocomplete_request(""), autocomplete.clone())
        .await
        .expect("trinket autocomplete");
    assert_eq!(
        autocomplete
            .autocompletes
            .lock()
            .expect("autocomplete records")
            .len(),
        1
    );

    let status = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(status_command(), status.clone())
        .await
        .expect("persistent status panel");
    let click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(
            component(status.button_id_containing(":buy:")),
            click.clone(),
        )
        .await
        .expect("persistent status refresh");
    assert_eq!(status.edits.lock().expect("status edits").len(), 1);
    assert_eq!(
        click.defers.lock().expect("component defers").as_slice(),
        &[false]
    );
}

#[test]
fn battle_timeout_window_belongs_to_the_round_not_the_battle() {
    let turn = Duration::from_secs(30);
    let short = Duration::from_secs(1);

    // A round that only just started is never expired, however long the battle
    // as a whole has been running.
    assert_eq!(
        battle_timeout_step(Some(1), Some(1), short, turn),
        BattleTimeoutStep::Wait
    );
    // Its own full window having passed does expire it.
    assert_eq!(
        battle_timeout_step(Some(1), Some(1), turn, turn),
        BattleTimeoutStep::Fire
    );
    // When the players resolve a round themselves, the next one restarts the
    // window rather than inheriting the remainder of the previous one.
    assert_eq!(
        battle_timeout_step(Some(1), Some(2), turn, turn),
        BattleTimeoutStep::Rearm
    );
    // A battle that has ended stops the task.
    assert_eq!(
        battle_timeout_step(Some(2), None, turn, turn),
        BattleTimeoutStep::Stop
    );
}
#[tokio::test]
async fn a_void_tombstone_still_tells_the_clicker_why_the_brawl_died() {
    let (database, provider, challenger_pet, _recipient_pet) = brawl_fixture();
    let initial = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(brawl_command(RECIPIENT, 0), initial.clone())
        .await
        .expect("challenge delivery");
    rusqlite::Connection::open(database.path())
        .expect("open brawl fixture")
        .execute(
            "UPDATE pets SET died_at=?1 WHERE pet_id=?2",
            rusqlite::params![SystemPetClock.now(), challenger_pet],
        )
        .expect("bury the challenger's pet");
    let accept = initial.first_button_id();
    let click = Arc::new(TestResponder::new(false));
    provider
        .handler
        .handle(component_as(accept, RECIPIENT), click.clone())
        .await
        .expect("accept a dead pet's challenge");

    assert_eq!(
        click.updates.lock().expect("click updates").len(),
        1,
        "the challenge message is replaced by the void tombstone"
    );
    let followups = click.followups.lock().expect("click followups");
    let reason = followups
        .first()
        .expect("the clicker is told why the challenge voided");
    assert!(
        reason.content.contains("no longer with us"),
        "unexpected failure notice: {:?}",
        reason.content
    );
    assert!(reason.ephemeral, "the failure notice is private");
    assert_eq!(click.call_log(), vec!["update", "followup"]);
}
