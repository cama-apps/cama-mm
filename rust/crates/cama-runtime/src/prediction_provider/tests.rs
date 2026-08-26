use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cama_db::low_priority_repository::SetLowPriorityInput;
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordIdPlayerNameResolver, DiscordMessageReceipt,
    DiscordMessageSnapshot, GuildPlayerNameDirectory, GuildPlayerNameResolver,
};
use crate::gamba_guild_source::GambaDestination;
use crate::registration::{InteractionResponseError, Registry};

const GUILD: i64 = 42;
const GAMBA_CHANNEL: i64 = 420;
const COMMAND_CHANNEL: i64 = 421;
const THREAD_ID: i64 = 700;
const EMBED_MESSAGE_ID: i64 = 800;
const CHANNEL_MESSAGE_ID: i64 = 900;
const ADMIN: i64 = 99;
const TRADER: i64 = 100;

#[derive(Default)]
struct CapturedResponses {
    immediate: Vec<InteractionResponse>,
    deferred: Vec<bool>,
    followups: Vec<InteractionResponse>,
    modals: Vec<InteractionModal>,
}

#[derive(Default)]
struct CapturingResponder {
    captured: Mutex<CapturedResponses>,
}

impl CapturingResponder {
    fn snapshot(&self) -> CapturedResponses {
        let captured = self.captured.lock().expect("response capture");
        CapturedResponses {
            immediate: captured.immediate.clone(),
            deferred: captured.deferred.clone(),
            followups: captured.followups.clone(),
            modals: captured.modals.clone(),
        }
    }
}

#[async_trait]
impl InteractionResponder for CapturingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .immediate
            .push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .deferred
            .push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .followups
            .push(response);
        Ok(())
    }

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .modals
            .push(modal);
        Ok(())
    }
}

#[derive(Default)]
struct FakeVanityTax;

impl PredictionVanityTaxPort for FakeVanityTax {
    fn taxable_ids(&self, _guild_id: i64) -> BTreeSet<i64> {
        BTreeSet::new()
    }
}

struct FixedPlayerNames(GuildPlayerNameDirectory);

impl GuildPlayerNameResolver for FixedPlayerNames {
    fn names_for_guild(
        &self,
        _guild_id: i64,
        _player_ids: &[i64],
    ) -> Result<GuildPlayerNameDirectory, String> {
        Ok(self.0.clone())
    }
}

#[derive(Clone)]
struct FakeGambaGuilds(Vec<GambaDestination>);

impl Default for FakeGambaGuilds {
    fn default() -> Self {
        Self(vec![GambaDestination {
            guild_id: GUILD,
            channel_id: GAMBA_CHANNEL,
        }])
    }
}

impl GambaGuildSource for FakeGambaGuilds {
    fn live_gamba_destinations(&self) -> Result<Vec<GambaDestination>, String> {
        Ok(self.0.clone())
    }
}

#[derive(Default)]
struct FakeCommandDiscord {
    is_gamba: AtomicBool,
    created: Mutex<Vec<(i64, DiscordMessage, String, DiscordMessage)>>,
    edits: Mutex<Vec<(i64, i64, DiscordMessage)>>,
    reopened: Mutex<Vec<i64>>,
    archived: Mutex<Vec<i64>>,
}

impl FakeCommandDiscord {
    fn accepting_gamba() -> Self {
        Self {
            is_gamba: AtomicBool::new(true),
            ..Self::default()
        }
    }

    fn set_gamba(&self, value: bool) {
        self.is_gamba.store(value, Ordering::SeqCst);
    }
}

#[async_trait]
impl PredictionCommandDiscordPort for FakeCommandDiscord {
    async fn prediction_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String> {
        assert_eq!(guild_id, GUILD);
        assert!(channel_id == COMMAND_CHANNEL || channel_id == THREAD_ID);
        Ok(self.is_gamba.load(Ordering::SeqCst))
    }

    async fn prediction_display_text(&self, _guild_id: i64, text: &str) -> Result<String, String> {
        Ok(text.replace("<@100>", "Trader"))
    }

    async fn create_prediction_market_surface(
        &self,
        channel_id: i64,
        announcement: DiscordMessage,
        thread_name: &str,
        market_message: DiscordMessage,
    ) -> Result<PredictionMarketSurface, String> {
        self.created.lock().expect("created surfaces").push((
            channel_id,
            announcement,
            thread_name.to_owned(),
            market_message,
        ));
        Ok(PredictionMarketSurface {
            channel_message_id: CHANNEL_MESSAGE_ID,
            thread_id: THREAD_ID,
            embed_message_id: EMBED_MESSAGE_ID,
        })
    }

    async fn edit_prediction_command_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.edits
            .lock()
            .expect("command edits")
            .push((thread_id, message_id, message));
        Ok(())
    }

    async fn reopen_prediction_thread(&self, thread_id: i64) -> Result<(), String> {
        self.reopened
            .lock()
            .expect("reopened threads")
            .push(thread_id);
        Ok(())
    }

    async fn archive_prediction_thread(&self, thread_id: i64) -> Result<(), String> {
        self.archived
            .lock()
            .expect("archived threads")
            .push(thread_id);
        Ok(())
    }
}

#[derive(Default)]
struct FakeMarketDiscord {
    edits: Mutex<Vec<(i64, i64, DiscordMessage)>>,
    messages: Mutex<Vec<(i64, DiscordMessage)>>,
}

#[derive(Default)]
struct FakePredictionNeon {
    events: Mutex<Vec<PredictionBigWin>>,
}

struct FixedPredictionNeonRandom(bool);

impl PredictionNeonRandomPort for FixedPredictionNeonRandom {
    fn roll(&self, _chance: f64) -> bool {
        self.0
    }
}

#[derive(Default)]
struct ImmediatePredictionNeonSleep {
    durations: Mutex<Vec<Duration>>,
}

#[async_trait]
impl PredictionNeonSleepPort for ImmediatePredictionNeonSleep {
    async fn sleep(&self, duration: Duration) {
        self.durations
            .lock()
            .expect("prediction Neon sleeps")
            .push(duration);
    }
}

#[async_trait]
impl PredictionNeonPort for FakePredictionNeon {
    async fn on_big_win(&self, event: PredictionBigWin) -> Result<(), String> {
        self.events
            .lock()
            .expect("prediction Neon events")
            .push(event);
        Ok(())
    }
}

impl crate::first_game_pool_worker::FirstGamePoolGuildSource for FakeMarketDiscord {
    fn live_guild_ids(&self) -> Result<Vec<i64>, String> {
        Ok(vec![GUILD])
    }
}

#[async_trait]
impl PredictionDiscordPort for FakeMarketDiscord {
    async fn edit_prediction_market_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.edits
            .lock()
            .expect("market edits")
            .push((thread_id, message_id, message));
        Ok(())
    }

    async fn send_prediction_thread_message(
        &self,
        thread_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String> {
        self.messages
            .lock()
            .expect("thread messages")
            .push((thread_id, message));
        Ok(())
    }
}

#[derive(Default)]
struct FakeDiscordTransport {
    next_message: AtomicU64,
    sends: Mutex<Vec<(u64, DiscordMessage)>>,
    deletes: Mutex<Vec<(u64, u64)>>,
}

#[async_trait]
impl DiscordTransport for FakeDiscordTransport {
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
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        self.sends
            .lock()
            .expect("channel sends")
            .push((channel_id, message));
        let message_id = self.next_message.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(DiscordMessageReceipt {
            channel_id,
            message_id,
            jump_url: format!("https://discord.com/channels/{GUILD}/{channel_id}/{message_id}"),
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

    async fn delete_message(&self, channel_id: u64, message_id: u64) -> Result<(), String> {
        self.deletes
            .lock()
            .expect("channel deletes")
            .push((channel_id, message_id));
        Ok(())
    }

    async fn create_public_thread(
        &self,
        _channel_id: u64,
        _message_id: u64,
        _name: &str,
    ) -> Result<u64, String> {
        Ok(u64::try_from(THREAD_ID).expect("positive thread"))
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
}

struct Fixture {
    _directory: TempDir,
    path: PathBuf,
    command: Arc<FakeCommandDiscord>,
    market: Arc<FakeMarketDiscord>,
    discord: Arc<FakeDiscordTransport>,
    neon: Arc<FakePredictionNeon>,
}

impl Fixture {
    fn migrated() -> Self {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("cama.db");
        initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
            .expect("migrate temporary database");
        Self {
            _directory: directory,
            path,
            command: Arc::new(FakeCommandDiscord::accepting_gamba()),
            market: Arc::new(FakeMarketDiscord::default()),
            discord: Arc::new(FakeDiscordTransport::default()),
            neon: Arc::new(FakePredictionNeon::default()),
        }
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(&self.path).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match runtime foreign-key policy");
        connection
    }

    fn player(&self, discord_id: i64, balance: i64) {
        self.connection()
            .execute(
                "INSERT INTO players(
                    discord_id,guild_id,discord_username,jopacoin_balance,lowest_balance_ever
                 ) VALUES(?1,?2,?3,?4,?4)",
                params![discord_id, GUILD, format!("player-{discord_id}"), balance],
            )
            .expect("insert prediction player");
    }

    fn market(&self, question: &str) -> i64 {
        let repository = PredictionRepository::new(&self.path);
        let prediction_id = repository
            .create_orderbook_prediction(
                Some(GUILD),
                ADMIN,
                question,
                50,
                Some(COMMAND_CHANNEL),
                &[
                    NewLevel::ask(51, 20),
                    NewLevel::ask(52, 20),
                    NewLevel::bid(49, 20),
                    NewLevel::bid(48, 20),
                ],
            )
            .expect("create prediction fixture");
        repository
            .update_discord_ids(
                prediction_id,
                Some(THREAD_ID),
                Some(EMBED_MESSAGE_ID),
                Some(CHANNEL_MESSAGE_ID),
            )
            .expect("persist prediction Discord IDs");
        prediction_id
    }

    fn provider(&self) -> PredictionRegistrationProvider {
        self.provider_with_config(test_config())
    }

    fn provider_with_config(
        &self,
        config: PredictionCommandConfig,
    ) -> PredictionRegistrationProvider {
        PredictionRegistrationProvider {
            handler: Arc::new(PredictionInteractionHandler {
                predictions: PredictionRepository::new(&self.path),
                resolutions: PredictionResolutionRepository::new(&self.path),
                pets: PetEvolutionRepository::new(&self.path),
                low_priority: LowPriorityRepository::new(&self.path),
                config,
                vanity_tax: Arc::new(FakeVanityTax),
                command_discord: self.command.clone(),
                market_discord: self.market.clone(),
                gamba_guilds: Arc::new(FakeGambaGuilds::default()),
                discord: self.discord.clone(),
                neon: self.neon.clone(),
                player_names: Arc::new(DiscordIdPlayerNameResolver),
                help_cooldowns: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    fn registry(&self) -> Registry {
        let mut builder = RegistryBuilder::default();
        builder
            .add_provider(&self.provider())
            .expect("register prediction provider");
        builder.build()
    }
}

#[tokio::test]
async fn recent_trades_prefer_server_nickname_over_transport_display_name() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let prediction_id = fixture.market("Will the trade tape use the server nickname?");
    PredictionRepository::new(&fixture.path)
        .buy_contracts_atomic_with_max(prediction_id, TRADER, ContractSide::Yes, 2, 100)
        .expect("buy visible trade");
    let provider = fixture
        .provider()
        .with_player_names(Arc::new(FixedPlayerNames(GuildPlayerNameDirectory::new(
            Some(BTreeMap::from([(
                TRADER as u64,
                "Server Trader".to_owned(),
            )])),
        ))));
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&provider)
        .expect("register prediction provider");
    let handler = builder
        .build()
        .command_handler("predict")
        .expect("predict command handler");
    let responder = Arc::new(CapturingResponder::default());

    handler
        .handle(
            command_request(
                "view",
                vec![integer("prediction_id", prediction_id)],
                TRADER,
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("prediction view");

    let captured = responder.snapshot();
    let recent = captured.followups[0].embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "Recent")
        .expect("recent trades field");
    assert!(recent.value.contains("Server Trader"));
    assert!(!recent.value.contains("Player 100"));
}

fn test_config() -> PredictionCommandConfig {
    PredictionCommandConfig {
        admin_user_ids: BTreeSet::from([ADMIN]),
        bankruptcy_credit_bps: 7_500,
        contract_value: 100,
        drift_min: -2,
        drift_max: 2,
        economy_events_enabled: false,
        fade_ticks: 5,
        initial_fair_default: 50,
        levels_per_side: 2,
        low_priority_tax_bps: 0,
        max_contracts_per_trade: 100,
        outer_level_sizes: vec![],
        price_high: 95,
        price_low: 5,
        recent_trades_shown: 5,
        refresh_levels_per_side: 2,
        refresh_outer_level_sizes: vec![],
        refresh_seconds: 86_400,
        refresh_size_per_level: 20,
        refresh_spread_ticks: 2,
        size_per_level: 20,
        spread_ticks: 2,
        tick_size: 1,
        vanity_tax_bps: 0,
    }
}

fn command_request(
    subcommand: &str,
    options: Vec<InteractionOption>,
    user_id: i64,
    permissions: Option<u64>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "predict".to_owned(),
        user_id: u64::try_from(user_id).expect("positive user ID"),
        user_display_name: format!("player-{user_id}"),
        guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
        channel_id: Some(u64::try_from(COMMAND_CHANNEL).expect("positive channel ID")),
        member_permissions: permissions,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn integer(name: &str, value: i64) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::Integer(value),
    }
}

fn string(name: &str, value: &str) -> InteractionOption {
    InteractionOption {
        name: name.to_owned(),
        value: InteractionValue::String(value.to_owned()),
    }
}

fn all_options_disable_autocomplete(options: &[CommandOptionSpec]) -> bool {
    options
        .iter()
        .all(|option| !option.autocomplete && all_options_disable_autocomplete(&option.options))
}

#[test]
fn registers_the_complete_python_predict_tree_and_persistent_component_prefix() {
    let fixture = Fixture::migrated();
    let registry = fixture.registry();
    let commands = registry.commands().collect::<Vec<_>>();
    assert_eq!(commands.len(), 1);
    let command = commands[0];
    assert_eq!(command.name, "predict");
    assert_eq!(
        command
            .options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>(),
        [
            "create",
            "resolve",
            "rollback",
            "set_fair",
            "cancel",
            "list",
            "view",
            "help",
            "mine",
            "force_refresh",
            "refresh_status",
        ]
    );
    assert!(
        command
            .options
            .iter()
            .all(|option| option.kind == CommandOptionKind::Subcommand)
    );
    assert!(all_options_disable_autocomplete(&command.options));

    let outcome = command
        .options
        .iter()
        .find(|option| option.name == "resolve")
        .and_then(|resolve| {
            resolve
                .options
                .iter()
                .find(|option| option.name == "outcome")
        })
        .expect("resolve outcome option");
    assert!(outcome.required);
    assert_eq!(
        outcome.choices,
        [
            CommandOptionChoice::String {
                name: "YES".to_owned(),
                value: "yes".to_owned(),
            },
            CommandOptionChoice::String {
                name: "NO".to_owned(),
                value: "no".to_owned(),
            },
        ]
    );
    assert_eq!(registry.component_routes().len(), 1);
    assert_eq!(
        registry.component_routes()[0].custom_id_prefix,
        PREDICTION_COMPONENT_PREFIX
    );
    assert!(!registry.global_command_sync_enabled());
}

#[tokio::test]
async fn create_persists_only_real_surface_ids_and_installs_exact_persistent_buttons() {
    let fixture = Fixture::migrated();
    let registry = fixture.registry();
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request(
                "create",
                vec![
                    string("question", "Will <@100> win this market?"),
                    integer("initial_fair", 55),
                ],
                ADMIN,
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("create prediction");

    let responses = responder.snapshot();
    assert_eq!(responses.deferred, [true]);
    assert_eq!(responses.followups.len(), 1);
    assert!(responses.followups[0].ephemeral);
    assert_eq!(
        responses.followups[0].content,
        "✅ Market #1 created at YES 55%."
    );

    let market = PredictionRepository::new(&fixture.path)
        .get_prediction(1)
        .expect("read created market")
        .expect("created market");
    assert_eq!(market.thread_id, Some(THREAD_ID));
    assert_eq!(market.embed_message_id, Some(EMBED_MESSAGE_ID));
    assert_eq!(market.channel_message_id, Some(CHANNEL_MESSAGE_ID));
    assert_eq!(market.question, "Will <@100> win this market?");

    let created = fixture.command.created.lock().expect("created surfaces");
    assert_eq!(created.len(), 1);
    let (channel_id, announcement, thread_name, market_message) = &created[0];
    assert_eq!(*channel_id, COMMAND_CHANNEL);
    assert_eq!(
        announcement.allowed_mentions,
        DiscordAllowedMentions::Default
    );
    assert!(announcement.response.content.contains("opened by <@99>"));
    assert_eq!(thread_name, "Market #1: Will <@100> win this market?");
    assert_eq!(
        market_message.allowed_mentions,
        DiscordAllowedMentions::None
    );
    assert_eq!(market_message.response.components.len(), 1);
    assert_eq!(
        market_message.response.components[0]
            .buttons
            .iter()
            .map(|button| (
                button.custom_id.as_str(),
                button.label.as_str(),
                button.emoji.as_deref(),
                button.style,
            ))
            .collect::<Vec<_>>(),
        [
            (
                BUY_YES_ID,
                "BUY YES",
                Some("🟢"),
                InteractionButtonStyle::Success,
            ),
            (
                BUY_NO_ID,
                "BUY NO",
                Some("🔴"),
                InteractionButtonStyle::Danger,
            ),
            (
                MY_POSITION_ID,
                "MY POSITION",
                Some("📊"),
                InteractionButtonStyle::Secondary,
            ),
        ]
    );
    assert_eq!(market_message.response.attachments.len(), 1);
    assert!(
        market_message.response.attachments[0]
            .bytes
            .starts_with(b"\x89PNG")
    );
}

#[tokio::test]
async fn wrong_channel_charges_one_coin_and_returns_python_denial_copy() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 10);
    fixture.command.set_gamba(false);
    let registry = fixture.registry();
    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request("list", vec![], TRADER, None),
            responder.clone(),
        )
        .await
        .expect("wrong-channel policy");

    let responses = responder.snapshot();
    assert_eq!(responses.immediate.len(), 1);
    assert!(responses.immediate[0].ephemeral);
    assert_eq!(responses.immediate[0].content, WRONG_CHANNEL_MESSAGE);
    let (balance, lowest): (i64, i64) = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance,lowest_balance_ever FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![TRADER, GUILD],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read charged wallet");
    assert_eq!((balance, lowest), (9, 9));
}

#[tokio::test]
async fn fixed_button_and_encoded_modal_survive_a_provider_restart() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let prediction_id = fixture.market("Will the restart keep this button alive?");

    let first_registry = fixture.registry();
    let modal_responder = Arc::new(CapturingResponder::default());
    first_registry
        .component_handler(BUY_YES_ID)
        .expect("persistent buy component")
        .handle(
            InteractionRequest::Component {
                interaction_id: 2,
                custom_id: BUY_YES_ID.to_owned(),
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                user_display_name: "Prediction Trader".to_owned(),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                values: vec![],
            },
            modal_responder.clone(),
        )
        .await
        .expect("open durable buy modal");
    let modal = modal_responder
        .snapshot()
        .modals
        .into_iter()
        .next()
        .expect("buy modal");
    assert_eq!(
        modal.custom_id,
        format!("{BUY_MODAL_PREFIX}{THREAD_ID}:yes")
    );
    assert_eq!(modal.title, "Buy YES contracts");

    // Rebuild the complete provider to model a process restart. The modal ID
    // carries the durable thread key and does not depend on an in-memory view.
    let restarted_registry = fixture.registry();
    let buy_responder = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(&modal.custom_id)
        .expect("encoded modal route")
        .handle(
            InteractionRequest::Modal {
                interaction_id: 3,
                custom_id: modal.custom_id,
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                fields: BTreeMap::from([("contracts".to_owned(), "3".to_owned())]),
            },
            buy_responder.clone(),
        )
        .await
        .expect("submit durable buy modal");

    let responses = buy_responder.snapshot();
    assert_eq!(responses.deferred, [true]);
    assert_eq!(responses.followups.len(), 1);
    assert!(responses.followups[0].ephemeral);
    assert!(responses.followups[0].content.contains("Bought **3 YES**"));
    let position = PredictionRepository::new(&fixture.path)
        .get_position(prediction_id, TRADER)
        .expect("read durable position")
        .expect("trader position");
    assert_eq!(position.yes_contracts, 3);
    assert_eq!(position.yes_cost_basis_total, 15);
    let edits = fixture.market.edits.lock().expect("market edits");
    assert_eq!(edits.len(), 1);
    assert_eq!((edits[0].0, edits[0].1), (THREAD_ID, EMBED_MESSAGE_ID));
    assert_eq!(edits[0].2.response.components.len(), 1);
    assert_eq!(
        edits[0].2.response.components[0].buttons[0].custom_id,
        BUY_YES_ID
    );
}

#[tokio::test]
async fn recreated_provider_rejects_stale_prediction_callbacks_without_duplicate_side_effects() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let prediction_id = fixture.market("Will a stale callback stay harmless?");

    // The first provider instance owns the interaction that was rendered
    // before the process boundary.  The modal ID is the durable thread key,
    // not an in-memory view handle.
    let first_registry = fixture.registry();
    let first_responder = Arc::new(CapturingResponder::default());
    first_registry
        .component_handler(BUY_YES_ID)
        .expect("persistent buy component")
        .handle(
            InteractionRequest::Component {
                interaction_id: 10,
                custom_id: BUY_YES_ID.to_owned(),
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                user_display_name: "Prediction Trader".to_owned(),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                values: vec![],
            },
            first_responder.clone(),
        )
        .await
        .expect("open first-process buy modal");
    let stale_modal = first_responder
        .snapshot()
        .modals
        .into_iter()
        .next()
        .expect("first-process buy modal");
    assert_eq!(
        stale_modal.custom_id,
        format!("{BUY_MODAL_PREFIX}{THREAD_ID}:yes")
    );
    drop(first_registry);

    // A reconstructed provider can still resolve the fixed button through
    // the current SQLite row, proving restart-safe resumption before the
    // market is closed.
    let restarted_registry = fixture.registry();
    let resumed_responder = Arc::new(CapturingResponder::default());
    restarted_registry
        .component_handler(BUY_YES_ID)
        .expect("restarted persistent buy component")
        .handle(
            InteractionRequest::Component {
                interaction_id: 11,
                custom_id: BUY_YES_ID.to_owned(),
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                user_display_name: "Prediction Trader".to_owned(),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                values: vec![],
            },
            resumed_responder.clone(),
        )
        .await
        .expect("resume persistent buy component");
    let resumed_modal = resumed_responder
        .snapshot()
        .modals
        .into_iter()
        .next()
        .expect("restarted buy modal");
    assert_eq!(resumed_modal.custom_id, stale_modal.custom_id);

    // Close the market through the same composed provider.  This is the
    // production state transition that makes both the rendered button and
    // previously-issued modal stale.
    restarted_registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request(
                "resolve",
                vec![
                    integer("prediction_id", prediction_id),
                    string("outcome", "yes"),
                ],
                ADMIN,
                None,
            ),
            Arc::new(CapturingResponder::default()),
        )
        .await
        .expect("resolve market before stale callback");
    assert_eq!(
        PredictionRepository::new(&fixture.path)
            .get_prediction(prediction_id)
            .expect("read resolved market")
            .expect("resolved market")
            .status,
        "resolved"
    );
    assert_eq!(
        fixture.command.edits.lock().expect("terminal edits").len(),
        1,
        "settlement performs one terminal market edit"
    );

    // Recreate the provider once more and deliver the old modal callback.
    // The repository's atomic open-market guard must reject it before any
    // wallet/position mutation or market refresh.
    let stale_registry = fixture.registry();
    let stale_modal_responder = Arc::new(CapturingResponder::default());
    stale_registry
        .component_handler(&stale_modal.custom_id)
        .expect("encoded stale modal route")
        .handle(
            InteractionRequest::Modal {
                interaction_id: 12,
                custom_id: stale_modal.custom_id.clone(),
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                fields: BTreeMap::from([("contracts".to_owned(), "3".to_owned())]),
            },
            stale_modal_responder.clone(),
        )
        .await
        .expect("stale modal is handled safely");
    let stale_modal_responses = stale_modal_responder.snapshot();
    assert_eq!(stale_modal_responses.deferred, [true]);
    assert_eq!(stale_modal_responses.followups.len(), 1);
    assert!(
        stale_modal_responses.followups[0]
            .content
            .contains("Market is not open for trading.")
    );
    assert!(
        PredictionRepository::new(&fixture.path)
            .get_position(prediction_id, TRADER)
            .expect("read stale position")
            .is_none()
    );
    let balance: i64 = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![TRADER, GUILD],
            |row| row.get(0),
        )
        .expect("read stale wallet");
    assert_eq!(balance, 1_000);
    assert_eq!(
        fixture.command.edits.lock().expect("terminal edits").len(),
        1,
        "stale modal does not duplicate the terminal Discord edit"
    );
    assert!(
        fixture
            .market
            .edits
            .lock()
            .expect("market edits")
            .is_empty()
    );

    // Buttons open their modal without database/network work. The durable
    // market check happens on submit, so even a stale button is acknowledged
    // immediately and cannot trigger a Discord edit by itself.
    let stale_button_responder = Arc::new(CapturingResponder::default());
    stale_registry
        .component_handler(BUY_YES_ID)
        .expect("stale persistent buy component")
        .handle(
            InteractionRequest::Component {
                interaction_id: 13,
                custom_id: BUY_YES_ID.to_owned(),
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                user_display_name: "Prediction Trader".to_owned(),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                values: vec![],
            },
            stale_button_responder.clone(),
        )
        .await
        .expect("stale fixed button is handled safely");
    let stale_button_responses = stale_button_responder.snapshot();
    assert!(stale_button_responses.immediate.is_empty());
    assert_eq!(stale_button_responses.modals.len(), 1);
    assert_eq!(
        fixture.command.edits.lock().expect("terminal edits").len(),
        1,
        "stale fixed button does not duplicate the terminal Discord edit"
    );
}

#[tokio::test]
async fn resolve_rollback_cancel_drive_durable_state_and_terminal_or_restored_components() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let prediction_id = fixture.market("Will the full lifecycle settle correctly?");
    PredictionRepository::new(&fixture.path)
        .buy_contracts_atomic_with_max(prediction_id, TRADER, ContractSide::Yes, 2, 100)
        .expect("buy lifecycle contracts");
    let registry = fixture.registry();

    let resolve_responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request(
                "resolve",
                vec![
                    integer("prediction_id", prediction_id),
                    string("outcome", "yes"),
                ],
                ADMIN,
                None,
            ),
            resolve_responder.clone(),
        )
        .await
        .expect("resolve prediction");
    let resolve_responses = resolve_responder.snapshot();
    assert_eq!(resolve_responses.deferred, [false]);
    assert!(
        resolve_responses.followups[0]
            .content
            .contains("resolved **YES**")
    );
    assert_eq!(
        PredictionRepository::new(&fixture.path)
            .get_prediction(prediction_id)
            .expect("read resolved market")
            .expect("resolved market")
            .status,
        "resolved"
    );
    let settled_balance: i64 = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![TRADER, GUILD],
            |row| row.get(0),
        )
        .expect("read settled wallet");
    assert_eq!(settled_balance, 1_190);
    assert_eq!(
        *fixture.neon.events.lock().expect("prediction Neon events"),
        [PredictionBigWin {
            discord_id: TRADER,
            guild_id: GUILD,
            channel_id: COMMAND_CHANNEL,
            payout: 200,
        }]
    );

    {
        let edits = fixture.command.edits.lock().expect("command edits");
        assert_eq!(edits.len(), 1);
        assert!(edits[0].2.response.components.is_empty());
        assert_eq!(edits[0].2.response.embeds[0].fields[0].value, "**YES**");
    }
    assert_eq!(
        *fixture.command.archived.lock().expect("archived threads"),
        [THREAD_ID]
    );
    assert!(
        fixture.market.messages.lock().expect("thread messages")[0]
            .1
            .response
            .content
            .contains("resolved **YES**")
    );
    assert_eq!(
        fixture.discord.sends.lock().expect("gamba sends")[0].0,
        u64::try_from(GAMBA_CHANNEL).expect("positive gamba ID")
    );

    let rollback_responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request(
                "rollback",
                vec![integer("prediction_id", prediction_id)],
                ADMIN,
                None,
            ),
            rollback_responder,
        )
        .await
        .expect("rollback prediction");
    let reopened = PredictionRepository::new(&fixture.path)
        .get_prediction(prediction_id)
        .expect("read reopened market")
        .expect("reopened market");
    assert_eq!(reopened.status, "open");
    assert_eq!(
        *fixture.command.reopened.lock().expect("reopened threads"),
        [THREAD_ID]
    );
    {
        let edits = fixture.command.edits.lock().expect("command edits");
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[1].2.response.components.len(), 1);
        assert_eq!(
            edits[1].2.response.components[0]
                .buttons
                .iter()
                .map(|button| button.custom_id.as_str())
                .collect::<Vec<_>>(),
            [BUY_YES_ID, BUY_NO_ID, MY_POSITION_ID]
        );
    }

    let cancel_responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request(
                "cancel",
                vec![integer("prediction_id", prediction_id)],
                ADMIN,
                None,
            ),
            cancel_responder,
        )
        .await
        .expect("cancel prediction");
    let cancelled = PredictionRepository::new(&fixture.path)
        .get_prediction(prediction_id)
        .expect("read cancelled market")
        .expect("cancelled market");
    assert_eq!(cancelled.status, "cancelled");
    let refunded_balance: i64 = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![TRADER, GUILD],
            |row| row.get(0),
        )
        .expect("read refunded wallet");
    assert_eq!(refunded_balance, 1_000);
    let edits = fixture.command.edits.lock().expect("command edits");
    assert_eq!(edits.len(), 3);
    assert!(edits[2].2.response.components.is_empty());
    assert_eq!(
        edits[2].2.response.embeds[0].fields[0].value,
        "**CANCELLED** — cost basis refunded."
    );
}

#[tokio::test]
async fn resolve_taxes_active_low_priority_profit_through_its_own_ledger_row() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let prediction_id = fixture.market("Does low priority pay its own tax?");
    PredictionRepository::new(&fixture.path)
        .buy_contracts_atomic_with_max(prediction_id, TRADER, ContractSide::Yes, 2, 100)
        .expect("buy taxed contracts");
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(&fixture.provider_with_config(PredictionCommandConfig {
            low_priority_tax_bps: 1_000,
            ..test_config()
        }))
        .expect("register prediction provider");
    let registry = builder.build();
    // Assigned after registration so the settlement has to read the live table.
    LowPriorityRepository::new(&fixture.path)
        .set_low_priority(&SetLowPriorityInput::new(TRADER, Some(GUILD), ADMIN, None))
        .expect("assign low priority");

    let responder = Arc::new(CapturingResponder::default());
    registry
        .command_handler("predict")
        .expect("predict command handler")
        .handle(
            command_request(
                "resolve",
                vec![
                    integer("prediction_id", prediction_id),
                    string("outcome", "yes"),
                ],
                ADMIN,
                None,
            ),
            responder.clone(),
        )
        .await
        .expect("resolve prediction");

    // 10 JC of cost basis against a 200 JC payout leaves 190 JC of profit.
    let settled_balance: i64 = fixture
        .connection()
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![TRADER, GUILD],
            |row| row.get(0),
        )
        .expect("read settled wallet");
    assert_eq!(settled_balance, 1_171);
    let tax_row: (i64, String) = fixture
        .connection()
        .query_row(
            "SELECT delta,reason FROM economy_ledger_entries
             WHERE guild_id=?1 AND account_id=?2 AND source='low_priority_tax'",
            params![GUILD, TRADER],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read low priority tax ledger");
    assert_eq!(tax_row, (-19, "low priority tax on JC profit".to_owned()));
    let vanity_rows: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM economy_ledger_entries
             WHERE guild_id=?1 AND source='vanity_tax'",
            [GUILD],
            |row| row.get(0),
        )
        .expect("read vanity tax ledger");
    assert_eq!(vanity_rows, 0);
    let persisted_tax: i64 = fixture
        .connection()
        .query_row(
            "SELECT low_priority_tax FROM prediction_positions
             WHERE prediction_id=?1 AND discord_id=?2",
            params![prediction_id, TRADER],
            |row| row.get(0),
        )
        .expect("read persisted low priority tax");
    assert_eq!(persisted_tax, 19);
    assert!(
        responder.snapshot().followups[0]
            .content
            .contains("(−19 JC low priority tax.)")
    );
}

#[tokio::test]
async fn production_prediction_neon_posts_real_media_and_deletes_after_exactly_sixty_seconds() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let sleep = Arc::new(ImmediatePredictionNeonSleep::default());
    let neon = ProductionPredictionNeonPort::with_test_ports(
        &fixture.path,
        PredictionNeonConfig {
            ai_features_default: false,
            bigwin_floor: 0.05,
            bigwin_full_payout: 5_000,
            bigwin_min_payout: 500,
            cooldown_seconds: 60,
            enabled: true,
        },
        fixture.discord.clone(),
        Arc::new(FixedPredictionNeonRandom(true)),
        sleep.clone(),
    );

    neon.on_big_win(PredictionBigWin {
        discord_id: TRADER,
        guild_id: GUILD,
        channel_id: COMMAND_CHANNEL,
        payout: 3_200,
    })
    .await
    .expect("send production prediction Neon");
    tokio::task::yield_now().await;

    let sends = fixture.discord.sends.lock().expect("channel sends");
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].0, u64::try_from(COMMAND_CHANNEL).unwrap());
    assert_eq!(sends[0].1.response.attachments.len(), 1);
    let attachment = &sends[0].1.response.attachments[0];
    assert_eq!(attachment.filename, "jopat_terminal.gif");
    let mut decoder = gif::DecodeOptions::new()
        .read_info(Cursor::new(&attachment.bytes))
        .expect("decode attached Neon GIF");
    assert_eq!((decoder.width(), decoder.height()), (400, 300));
    let mut durations = Vec::new();
    while let Some(frame) = decoder.read_next_frame().expect("decode Neon frame") {
        durations.push(u32::from(frame.delay) * 10);
    }
    assert_eq!(durations.len(), 40);
    assert_eq!(durations.last(), Some(&60_000));
    assert!(
        sends[0]
            .1
            .response
            .content
            .starts_with("```ansi\n[LEDGER] +3,200 JC")
    );
    drop(sends);

    assert_eq!(
        *sleep.durations.lock().expect("prediction Neon sleeps"),
        [Duration::from_secs(60)]
    );
    assert_eq!(
        *fixture.discord.deletes.lock().expect("channel deletes"),
        [(u64::try_from(COMMAND_CHANNEL).unwrap(), 1)]
    );
}

#[tokio::test]
async fn production_prediction_neon_preserves_threshold_chance_and_cooldown_gates() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let neon = ProductionPredictionNeonPort::with_test_ports(
        &fixture.path,
        PredictionNeonConfig {
            ai_features_default: false,
            bigwin_floor: 0.05,
            bigwin_full_payout: 5_000,
            bigwin_min_payout: 500,
            cooldown_seconds: 60,
            enabled: true,
        },
        fixture.discord.clone(),
        Arc::new(FixedPredictionNeonRandom(true)),
        Arc::new(ImmediatePredictionNeonSleep::default()),
    );
    let base = PredictionBigWin {
        discord_id: TRADER,
        guild_id: GUILD,
        channel_id: COMMAND_CHANNEL,
        payout: 499,
    };
    neon.on_big_win(base).await.expect("below threshold no-op");
    neon.on_big_win(PredictionBigWin {
        payout: 500,
        ..base
    })
    .await
    .expect("threshold fires");
    neon.on_big_win(PredictionBigWin {
        payout: 5_000,
        ..base
    })
    .await
    .expect("cooldown no-op");
    assert_eq!(
        fixture.discord.sends.lock().expect("channel sends").len(),
        1
    );
    assert!((neon.chance(0) - 0.05).abs() < f64::EPSILON);
    assert!((neon.chance(5_000) - 0.95).abs() < f64::EPSILON);
    assert!((neon.chance(50_000) - 0.95).abs() < f64::EPSILON);
}

#[tokio::test]
async fn remaining_live_subcommands_and_position_button_preserve_python_visibility() {
    let fixture = Fixture::migrated();
    fixture.player(TRADER, 1_000);
    let prediction_id = fixture.market("Will every prediction command dispatch?");
    PredictionRepository::new(&fixture.path)
        .buy_contracts_atomic_with_max(prediction_id, TRADER, ContractSide::No, 2, 100)
        .expect("buy visible position");
    let registry = fixture.registry();
    let handler = registry
        .command_handler("predict")
        .expect("predict command handler");

    let help = Arc::new(CapturingResponder::default());
    handler
        .handle(command_request("help", vec![], TRADER, None), help.clone())
        .await
        .expect("prediction help");
    let help = help.snapshot();
    assert_eq!(help.deferred, [true]);
    assert!(help.followups[0].ephemeral);
    assert_eq!(
        help.followups[0].embeds[0].title.as_deref(),
        Some("📈 Prediction markets — quick guide")
    );

    let list = Arc::new(CapturingResponder::default());
    handler
        .handle(command_request("list", vec![], TRADER, None), list.clone())
        .await
        .expect("prediction list");
    let list = list.snapshot();
    assert_eq!(list.deferred, [false]);
    assert!(!list.followups[0].ephemeral);
    assert_eq!(
        list.followups[0].embeds[0].title.as_deref(),
        Some("📈 Open markets (1)")
    );

    let view = Arc::new(CapturingResponder::default());
    handler
        .handle(
            command_request(
                "view",
                vec![integer("prediction_id", prediction_id)],
                TRADER,
                None,
            ),
            view.clone(),
        )
        .await
        .expect("prediction view");
    let view = view.snapshot();
    assert_eq!(view.deferred, [false]);
    assert!(view.followups[0].components.is_empty());
    let market_embed = &view.followups[0].embeds[0];
    assert_eq!(
        market_embed.title.as_deref(),
        Some(format!("📈 Market #{prediction_id}")).as_deref()
    );
    assert_eq!(market_embed.color, Some(0x34_98_DB));
    assert_eq!(
        market_embed
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Current", "Order book", "Recent", "\u{200B}"]
    );
    assert!(market_embed.fields[1].value.starts_with("```ansi\n"));
    assert!(market_embed.fields[1].value.contains("🟢 Buy YES"));
    assert!(market_embed.fields[1].value.contains("🔴 Buy NO"));
    assert_eq!(
        market_embed.footer.as_deref(),
        Some("100 jopa per winning contract")
    );
    assert_eq!(view.followups[0].attachments.len(), 1);
    assert_eq!(
        market_embed.image_url.as_deref(),
        Some(format!("attachment://predict_{prediction_id}.png")).as_deref()
    );
    assert!(
        market_embed
            .fields
            .iter()
            .any(|field| field.value.starts_with("Your position:"))
    );

    let mine = Arc::new(CapturingResponder::default());
    handler
        .handle(command_request("mine", vec![], TRADER, None), mine.clone())
        .await
        .expect("prediction mine");
    let mine = mine.snapshot();
    assert_eq!(mine.deferred, [true]);
    assert_eq!(
        mine.followups[0].embeds[0].title.as_deref(),
        Some("📈 Your open positions")
    );

    let set_fair = Arc::new(CapturingResponder::default());
    handler
        .handle(
            command_request(
                "set_fair",
                vec![
                    integer("prediction_id", prediction_id),
                    integer("new_price", 60),
                ],
                ADMIN,
                None,
            ),
            set_fair.clone(),
        )
        .await
        .expect("prediction set fair");
    let set_fair = set_fair.snapshot();
    assert_eq!(set_fair.deferred, [false]);
    assert!(set_fair.followups[0].content.contains("YES 50% → 60%"));
    assert_eq!(
        set_fair.followups[0].allowed_mentions,
        crate::registration::InteractionAllowedMentions::Users(vec![
            u64::try_from(ADMIN).expect("positive admin ID")
        ])
    );

    let force = Arc::new(CapturingResponder::default());
    handler
        .handle(
            command_request(
                "force_refresh",
                vec![integer("prediction_id", prediction_id)],
                ADMIN,
                None,
            ),
            force.clone(),
        )
        .await
        .expect("prediction force refresh");
    let force = force.snapshot();
    assert_eq!(force.deferred, [true]);
    assert!(force.followups[0].content.contains("manually refreshed"));

    let status = Arc::new(CapturingResponder::default());
    handler
        .handle(
            command_request("refresh_status", vec![], ADMIN, None),
            status.clone(),
        )
        .await
        .expect("prediction refresh status");
    let status = status.snapshot();
    assert_eq!(status.deferred, [true]);
    assert_eq!(
        status.followups[0].embeds[0].title.as_deref(),
        Some("📈 Refresh status")
    );

    let position = Arc::new(CapturingResponder::default());
    registry
        .component_handler(MY_POSITION_ID)
        .expect("position component handler")
        .handle(
            InteractionRequest::Component {
                interaction_id: 4,
                custom_id: MY_POSITION_ID.to_owned(),
                user_id: u64::try_from(TRADER).expect("positive trader ID"),
                user_display_name: "Prediction Trader".to_owned(),
                guild_id: Some(u64::try_from(GUILD).expect("positive guild ID")),
                channel_id: Some(u64::try_from(THREAD_ID).expect("positive thread ID")),
                member_permissions: None,
                values: vec![],
            },
            position.clone(),
        )
        .await
        .expect("prediction position card");
    let position = position.snapshot();
    assert_eq!(position.immediate.len(), 1);
    assert!(position.immediate[0].ephemeral);
    assert!(position.immediate[0].components.is_empty());
    assert_eq!(
        position.immediate[0].embeds[0].title.as_deref(),
        Some("Your position in market #1")
    );
}

#[test]
fn duration_copy_matches_python_zero_floor() {
    assert_eq!(format_duration_short(-5), "1s");
    assert_eq!(format_duration_short(0), "1s");
    assert_eq!(format_duration_short(59), "59s");
    assert_eq!(format_duration_short(60), "1m");
}

#[test]
fn test_event_preserves_depth_while_modifying_new_and_refreshed_spreads() {
    let initial = configured_levels(50, 3, 50, 2, &[20, 15, 10, 5], 1, 2);
    let initial_asks = initial
        .iter()
        .filter(|level| level.side.as_str() == "yes_ask")
        .map(|level| (level.price, level.size))
        .collect::<Vec<_>>();
    assert_eq!(
        initial_asks,
        [
            (54, 50),
            (55, 50),
            (56, 50),
            (57, 20),
            (58, 15),
            (59, 10),
            (60, 5)
        ]
    );

    // A refresh has its own depth/taper configuration.  The daily event only
    // widens the base spread; it must not scale or truncate liquidity.
    let refreshed = configured_levels(48, 2, 50, 4, &[60, 30, 25, 18, 11, 4, 2], 1, 2);
    let refreshed_asks = refreshed
        .iter()
        .filter(|level| level.side.as_str() == "yes_ask")
        .map(|level| (level.price, level.size))
        .collect::<Vec<_>>();
    assert_eq!(
        refreshed_asks,
        [
            (54, 50),
            (55, 50),
            (56, 60),
            (57, 30),
            (58, 25),
            (59, 18),
            (60, 11),
            (61, 4),
            (62, 2),
        ]
    );
    assert_eq!(adjusted_spread(4, 2, 1), 6);
}

#[test]
fn test_event_ladder_spread_is_clamped_without_changing_depth() {
    let levels = configured_levels(50, 3, 50, 2, &[20, 15, 10, 5], 1, -999);
    let asks = levels
        .iter()
        .filter(|level| level.side.as_str() == "yes_ask")
        .map(|level| (level.price, level.size))
        .collect::<Vec<_>>();
    assert_eq!(
        asks,
        [
            (51, 50),
            (52, 50),
            (53, 50),
            (54, 20),
            (55, 15),
            (56, 10),
            (57, 5)
        ]
    );
    assert_eq!(adjusted_spread(2, -999, 1), 1);
}
