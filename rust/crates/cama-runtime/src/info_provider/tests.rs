use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use cama_db::schema_manager::{MigrationSettings, initialize_or_migrate_with_settings};
use cama_db::tip_repository::TipRepository;
use cama_db::trivia_commands_repository::TriviaCommandsRepository;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use super::*;
use crate::Registry;
use crate::discord_transport::{
    DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessage, DiscordMessageReceipt,
    DiscordMessageSnapshot, DiscordPresence,
};
use crate::registration::{InteractionMessageDelivery, InteractionResponseError};

const GUILD: u64 = 7_001;
const CHANNEL: u64 = 8_001;
const ALICE: u64 = 101;
const BOB_DEPARTED: u64 = 202;
const CAROL: u64 = 303;

#[derive(Default)]
struct RecordingDiscord {
    members: BTreeMap<(u64, u64), DiscordGuildMemberSnapshot>,
    member_lookups: Mutex<Vec<(u64, u64)>>,
}

impl RecordingDiscord {
    fn with_members(members: impl IntoIterator<Item = (u64, &'static str)>) -> Self {
        Self {
            members: members
                .into_iter()
                .map(|(user_id, display_name)| {
                    (
                        (GUILD, user_id),
                        DiscordGuildMemberSnapshot {
                            user_id,
                            display_name: display_name.to_owned(),
                            presence: DiscordPresence::Online,
                            in_voice: false,
                            deafened: false,
                            activities: Vec::new(),
                        },
                    )
                })
                .collect(),
            member_lookups: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl DiscordTransport for RecordingDiscord {
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
            jump_url: "https://discord.test/message/1".to_owned(),
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
        guild_id: u64,
        user_id: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        self.member_lookups
            .lock()
            .expect("member lookups")
            .push((guild_id, user_id));
        Ok(self.members.get(&(guild_id, user_id)).cloned())
    }
}

#[derive(Default)]
struct RecordingResponder {
    responses: Mutex<Vec<InteractionResponse>>,
    deferrals: Mutex<Vec<bool>>,
    followups: Mutex<Vec<InteractionResponse>>,
    updates: Mutex<Vec<InteractionResponse>>,
    original_edits: Mutex<Vec<InteractionResponse>>,
    deletions: Mutex<Vec<InteractionMessageReceipt>>,
    receipt: Mutex<Option<InteractionMessageReceipt>>,
}

impl RecordingResponder {
    fn with_receipt(message_id: u64) -> Self {
        Self {
            receipt: Mutex::new(Some(InteractionMessageReceipt {
                message_id,
                channel_id: CHANNEL,
                delivery: InteractionMessageDelivery::InteractionFollowup,
            })),
            ..Self::default()
        }
    }
}

#[async_trait]
impl InteractionResponder for RecordingResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.deferrals.lock().expect("deferrals").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        Ok(*self.receipt.lock().expect("receipt"))
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.updates.lock().expect("updates").push(response);
        Ok(())
    }

    async fn edit_original(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.original_edits
            .lock()
            .expect("original edits")
            .push(response);
        Ok(())
    }

    async fn delete_message(
        &self,
        receipt: InteractionMessageReceipt,
    ) -> Result<(), InteractionResponseError> {
        self.deletions.lock().expect("deletions").push(receipt);
        Ok(())
    }
}

fn config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("test-token".to_owned()),
        "ADMIN_USER_IDS" => Some("42".to_owned()),
        "LEVERAGE_TIERS" => Some("7,9".to_owned()),
        "PINGEDASH_COST" => Some("123".to_owned()),
        "ECONOMY_EVENTS_ENABLED" => Some("false".to_owned()),
        _ => None,
    })
    .expect("info test configuration")
}

fn non_default_calibration_config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| {
        Some(
            match name {
                "DISCORD_BOT_TOKEN" => "test-token",
                "NEW_PLAYER_MMR_DISCOUNT" => "1000",
                "OPENSKILL_CALIBRATION_SIGMA_THRESHOLD" => "3",
                "OPENSKILL_PERFORMANCE_STRENGTH" => "0.25",
                "OPENSKILL_SIGMA_DECAY_GRACE_PERIOD_DAYS" => "11",
                "OPENSKILL_SIGMA_DECAY_PER_WEEK" => "1.25",
                "OPENSKILL_WIN_PROBABILITY_TEMPERATURE" => "2",
                _ => return None,
            }
            .to_owned(),
        )
    })
    .expect("non-default info test configuration")
}

fn migrated_database() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("temporary info database");
    let path = directory.path().join("cama.db");
    initialize_or_migrate_with_settings(&path, &MigrationSettings::default())
        .expect("migrate info database");
    (directory, path)
}

fn add_player(path: &Path, discord_id: u64, name: &str, balance: i64, rating: f64, os_mu: f64) {
    let mut player = NewPlayer::new(
        i64::try_from(discord_id).expect("small test user ID"),
        name,
        Some(i64::try_from(GUILD).expect("small test guild ID")),
    );
    player.initial_mmr = Some(4_000);
    player.glicko_rating = Some(rating);
    player.glicko_rd = Some(80.0);
    player.glicko_volatility = Some(0.06);
    player.os_mu = Some(os_mu);
    player.os_sigma = Some(4.0);
    PlayerRepository::new(path)
        .add(&player)
        .expect("insert production player fixture");
    Connection::open(path)
        .expect("open player fixture database")
        .execute(
            "UPDATE players SET jopacoin_balance=?1,wins=8,losses=2
              WHERE discord_id=?2 AND guild_id=?3",
            params![
                balance,
                i64::try_from(discord_id).expect("small test user ID"),
                i64::try_from(GUILD).expect("small test guild ID")
            ],
        )
        .expect("update production player fixture");
}

fn seed_leaderboard_data(path: &Path) {
    add_player(path, ALICE, "AliceStored", 10, 1_700.0, 40.0);
    add_player(path, BOB_DEPARTED, "BobStored", 50_000, 1_900.0, 44.0);
    add_player(path, CAROL, "CarolStored", 20, 1_800.0, 42.0);
    TipRepository::new(path)
        .log_tip(
            i64::try_from(ALICE).expect("small test user"),
            i64::try_from(CAROL).expect("small test user"),
            25,
            1,
            Some(i64::try_from(GUILD).expect("small test guild")),
        )
        .expect("record production tip fixture");
    TriviaCommandsRepository::new(path)
        .record_trivia_session(
            i64::try_from(ALICE).expect("small test user"),
            Some(i64::try_from(GUILD).expect("small test guild")),
            6,
            12,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time after epoch")
                .as_secs() as i64,
        )
        .expect("record production trivia fixture");
}

fn seed_gambling_leaderboard_data(path: &Path) {
    seed_leaderboard_data(path);
    let connection = Connection::open(path).expect("open gambling fixture database");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable legacy fixture foreign keys");
    for (match_id, winning_team) in [(1_i64, 1_i64), (2_i64, 2_i64), (3_i64, 1_i64)] {
        connection
            .execute(
                "INSERT INTO matches (
                     match_id, guild_id, team1_players, team2_players, winning_team
                 ) VALUES (?1, ?2, '[101]', '[202,303]', ?3)",
                params![
                    match_id,
                    i64::try_from(GUILD).expect("small test guild"),
                    winning_team
                ],
            )
            .expect("insert gambling match fixture");
    }
    let guild = i64::try_from(GUILD).expect("small test guild");
    let alice = i64::try_from(ALICE).expect("small test user");
    let bob = i64::try_from(BOB_DEPARTED).expect("small test user");
    let carol = i64::try_from(CAROL).expect("small test user");
    let bets = [
        // Alice: two wins and one loss, making the top-earners section.
        (1_i64, alice, "radiant", 10_i64, 20_i64),
        (2_i64, alice, "radiant", 20_i64, 0_i64),
        (3_i64, alice, "radiant", 30_i64, 60_i64),
        // Bob: one win and two losses, making the down-bad section.
        (1_i64, bob, "dire", 20_i64, 0_i64),
        (2_i64, bob, "dire", 40_i64, 40_i64),
        (3_i64, bob, "dire", 20_i64, 0_i64),
        // Carol: three wins and the largest total wager, making the
        // biggest-gamblers section as well.
        (1_i64, carol, "radiant", 15_i64, 30_i64),
        (2_i64, carol, "dire", 25_i64, 50_i64),
        (3_i64, carol, "radiant", 35_i64, 70_i64),
    ];
    for (match_id, discord_id, side, amount, payout) in bets {
        connection
            .execute(
                "INSERT INTO bets (
                     guild_id, match_id, discord_id, team_bet_on, amount,
                     leverage, bet_time, payout
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)",
                params![guild, match_id, discord_id, side, amount, match_id, payout],
            )
            .expect("insert gambling bet fixture");
    }
}

#[test]
fn info_analytics_snapshot_reads_all_production_leaderboard_sources() {
    let (_directory, path) = migrated_database();
    seed_leaderboard_data(&path);

    let snapshot = read_info_analytics_snapshot(&path, i64::try_from(GUILD).unwrap(), 100)
        .expect("read Info analytics snapshot");

    assert_eq!(snapshot.player_count, 3);
    assert_eq!(snapshot.balance_candidates, 3);
    assert_eq!(snapshot.glicko_candidates, 3);
    assert_eq!(snapshot.openskill_candidates, 3);
    assert_eq!(snapshot.gambling_entries, 0);
    assert_eq!(snapshot.tip_sender_entries, 1);
    assert_eq!(snapshot.tip_receiver_entries, 1);
    assert_eq!(snapshot.trivia_entries, 1);
}

fn registry(provider: &InfoRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register info provider");
    builder.build()
}

fn command_request(
    name: &str,
    user_id: u64,
    guild_id: Option<u64>,
    member_permissions: Option<u64>,
    options: Vec<InteractionOption>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: user_id.saturating_add(10_000),
        name: name.to_owned(),
        user_id,
        user_display_name: format!("User {user_id}"),
        guild_id,
        channel_id: Some(CHANNEL),
        member_permissions,
        options,
    }
}

fn component_request(custom_id: &str, user_id: u64) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: user_id.saturating_add(20_000),
        custom_id: custom_id.to_owned(),
        user_id,
        user_display_name: format!("Leaderboard User {user_id}"),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        values: Vec::new(),
    }
}

#[test]
fn command_schema_declares_every_live_info_surface_exactly() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let registry = registry(&provider);
    let commands = registry
        .commands()
        .map(|command| (command.name.as_str(), command))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(commands.len(), 3);
    assert_eq!(commands["help"].description, "List all available commands");
    assert!(commands["help"].options.is_empty());
    assert_eq!(
        commands["leaderboard"].description,
        "View leaderboard (balance, gambling, ratings)"
    );
    assert_eq!(commands["leaderboard"].options.len(), 2);
    assert_eq!(commands["leaderboard"].options[0].name, "type");
    assert_eq!(
        commands["leaderboard"].options[0].kind,
        CommandOptionKind::String
    );
    assert_eq!(
        commands["leaderboard"].options[0].choices,
        [
            string_choice("Balance", "balance"),
            string_choice("Glicko-2 Rating", "glicko"),
            string_choice("OpenSkill Rating", "openskill"),
            string_choice("Gambling", "gambling"),
            string_choice("Tips", "tips"),
            string_choice("Trivia", "trivia"),
        ]
    );
    assert_eq!(commands["leaderboard"].options[1].name, "limit");
    assert_eq!(
        commands["leaderboard"].options[1].kind,
        CommandOptionKind::Integer
    );
    assert!(!commands["leaderboard"].options[1].required);
    assert_eq!(commands["leaderboard"].options[1].min_integer, None);
    assert_eq!(commands["leaderboard"].options[1].max_integer, None);
    assert_eq!(commands["calibration"].options.len(), 1);
    assert_eq!(commands["calibration"].options[0].name, "user");
    assert_eq!(
        commands["calibration"].options[0].kind,
        CommandOptionKind::User
    );
    assert_eq!(registry.component_routes()[0].custom_id_prefix, "info:");
    assert_eq!(VIEW_TIMEOUT, Duration::from_secs(840));
}

#[test]
fn test_calibration_command_description_is_public() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let registry = registry(&provider);
    let calibration = registry
        .commands()
        .find(|command| command.name == "calibration")
        .expect("registered calibration command");

    assert_eq!(
        calibration.description,
        "View rating system stats and calibration progress"
    );
    assert!(!calibration.description.to_lowercase().contains("admin"));
}

#[tokio::test]
async fn test_calibration_command_has_no_admin_check() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let handler = registry(&provider)
        .command_handler("calibration")
        .expect("calibration handler");
    let responder = Arc::new(RecordingResponder::default());

    handler
        .handle(
            command_request("calibration", 99_999, Some(GUILD), None, Vec::new()),
            responder.clone(),
        )
        .await
        .expect("public calibration response");

    assert_eq!(*responder.deferrals.lock().expect("deferrals"), [true]);
    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups.len(), 1);
    assert!(followups[0].ephemeral);
    assert!(!followups[0].content.to_lowercase().contains("permission"));
}

#[tokio::test]
async fn test_calibration_command_docstring_is_public() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let handler = registry(&provider)
        .command_handler("help")
        .expect("help handler");
    let responder = Arc::new(RecordingResponder::default());

    handler
        .handle(
            command_request("help", 99_998, Some(GUILD), None, Vec::new()),
            responder.clone(),
        )
        .await
        .expect("public help response");

    let followups = responder.followups.lock().expect("followups");
    let calibration_help = followups[0].embeds[0]
        .fields
        .iter()
        .find(|field| field.value.contains("`/calibration`"))
        .expect("calibration help entry");
    assert!(
        calibration_help
            .value
            .contains("Rating system health & calibration stats")
    );
    assert!(!calibration_help.value.to_lowercase().contains("admin-only"));
}

#[tokio::test]
async fn leaderboard_default_is_balance_and_explicit_balance_matches() {
    let (_directory, path) = migrated_database();
    seed_leaderboard_data(&path);
    let provider = InfoRegistrationProvider::new(
        path,
        &config(),
        Arc::new(RecordingDiscord::with_members([
            (ALICE, "AliceLive"),
            (BOB_DEPARTED, "BobLive"),
            (CAROL, "CarolLive"),
        ])),
    );
    let handler = registry(&provider)
        .command_handler("leaderboard")
        .expect("leaderboard handler");

    for (user_id, options) in [
        (ALICE, Vec::new()),
        (
            CAROL,
            vec![InteractionOption {
                name: "type".to_owned(),
                value: InteractionValue::String("balance".to_owned()),
            }],
        ),
    ] {
        let responder = Arc::new(RecordingResponder::default());
        handler
            .handle(
                command_request("leaderboard", user_id, Some(GUILD), None, options),
                responder.clone(),
            )
            .await
            .expect("render balance leaderboard");
        let followups = responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        assert_eq!(
            followups[0].embeds[0].title.as_deref(),
            Some("LEADERBOARD > Balance")
        );
        assert!(!followups[0].ephemeral);
    }
}

#[tokio::test]
async fn leaderboard_type_gambling_routes_to_gambling_embed() {
    let (_directory, path) = migrated_database();
    seed_leaderboard_data(&path);
    let provider = InfoRegistrationProvider::new(
        path,
        &config(),
        Arc::new(RecordingDiscord::with_members([
            (ALICE, "AliceLive"),
            (BOB_DEPARTED, "BobLive"),
            (CAROL, "CarolLive"),
        ])),
    );
    let handler = registry(&provider)
        .command_handler("leaderboard")
        .expect("leaderboard handler");
    let responder = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request(
                "leaderboard",
                ALICE,
                Some(GUILD),
                None,
                vec![InteractionOption {
                    name: "type".to_owned(),
                    value: InteractionValue::String("gambling".to_owned()),
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("render gambling leaderboard");

    let followups = responder.followups.lock().expect("followups");
    assert_eq!(followups.len(), 1);
    assert_eq!(
        followups[0].embeds[0].title.as_deref(),
        Some("LEADERBOARD > Gambling")
    );
    assert!(
        followups[0].embeds[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains("No gambling data yet"))
    );
}

#[tokio::test]
async fn leaderboard_invalid_limits_are_clamped_to_safe_bounds() {
    let (_directory, path) = migrated_database();
    seed_leaderboard_data(&path);
    let provider = InfoRegistrationProvider::new(
        path,
        &config(),
        Arc::new(RecordingDiscord::with_members([
            (ALICE, "AliceLive"),
            (BOB_DEPARTED, "BobLive"),
            (CAROL, "CarolLive"),
        ])),
    );
    let handler = registry(&provider)
        .command_handler("leaderboard")
        .expect("leaderboard handler");

    for (user_id, limit) in [(ALICE, 150_i64), (CAROL, 0_i64)] {
        let responder = Arc::new(RecordingResponder::default());
        handler
            .handle(
                command_request(
                    "leaderboard",
                    user_id,
                    Some(GUILD),
                    None,
                    vec![InteractionOption {
                        name: "limit".to_owned(),
                        value: InteractionValue::Integer(limit),
                    }],
                ),
                responder.clone(),
            )
            .await
            .expect("render clamped leaderboard");
        let followups = responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        assert_eq!(
            followups[0].embeds[0].title.as_deref(),
            Some("LEADERBOARD > Balance")
        );
        assert!(followups[0].embeds[0].description.is_some());
    }
}

#[tokio::test]
async fn gambling_leaderboard_renders_all_sections_from_production_stats() {
    let (_directory, path) = migrated_database();
    seed_gambling_leaderboard_data(&path);
    let provider = InfoRegistrationProvider::new(
        path,
        &config(),
        Arc::new(RecordingDiscord::with_members([
            (ALICE, "AliceLive"),
            (BOB_DEPARTED, "BobLive"),
            (CAROL, "CarolLive"),
        ])),
    );
    let handler = registry(&provider)
        .command_handler("leaderboard")
        .expect("leaderboard handler");
    let responder = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request(
                "leaderboard",
                ALICE,
                Some(GUILD),
                None,
                vec![InteractionOption {
                    name: "type".to_owned(),
                    value: InteractionValue::String("gambling".to_owned()),
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("render populated gambling leaderboard");

    let followups = responder.followups.lock().expect("followups");
    let embed = &followups[0].embeds[0];
    let field_names = embed
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "Top Earners",
        "Down Bad",
        "Hall of Degen",
        "Biggest Gamblers",
    ] {
        assert!(
            field_names.contains(&expected),
            "missing {expected} section"
        );
    }
}

#[test]
fn production_gambling_stats_preserve_entry_shape_and_wager_sorting() {
    let (_directory, path) = migrated_database();
    seed_gambling_leaderboard_data(&path);
    let leaderboard = cama_db::gambling_stats_repository::GamblingStatsService::new(
        cama_db::gambling_stats_repository::GamblingStatsRepository::new(&path),
    )
    .get_leaderboard(Some(i64::try_from(GUILD).expect("small test guild")), 5, 3)
    .expect("production gambling leaderboard");

    assert!(leaderboard.biggest_gamblers.len() >= 2);
    assert!(
        leaderboard
            .biggest_gamblers
            .windows(2)
            .all(|entries| entries[0].total_wagered >= entries[1].total_wagered)
    );
    assert_eq!(leaderboard.server_stats.total_bets, 9);
    assert_eq!(leaderboard.server_stats.total_wagered, 215);
    assert_eq!(leaderboard.server_stats.unique_gamblers, 3);
    let entry = &leaderboard.biggest_gamblers[0];
    let _ = (
        entry.discord_id,
        entry.net_pnl,
        entry.win_rate,
        entry.total_bets,
    );
    let degen = &leaderboard.hall_of_degen[0];
    let _ = (degen.degen_score, degen.degen_title, degen.degen_emoji);
}

#[tokio::test]
async fn help_is_ephemeral_config_driven_and_permission_aware() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let handler = registry(&provider)
        .command_handler("help")
        .expect("help handler");
    let admin = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request("help", 42, Some(GUILD), None, Vec::new()),
            admin.clone(),
        )
        .await
        .expect("render admin help");

    assert_eq!(*admin.deferrals.lock().expect("deferrals"), [true]);
    {
        let followups = admin.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        assert!(followups[0].ephemeral);
        let embed = &followups[0].embeds[0];
        assert_eq!(embed.title.as_deref(), Some("📚 Cama Shuffle Bot Commands"));
        assert_eq!(embed.color, Some(DISCORD_BLUE));
        let betting = embed
            .fields
            .iter()
            .find(|field| field.name.starts_with("🎰 Betting"))
            .expect("betting help field");
        assert!(betting.value.contains("leverage: 7x, 9x"));
        assert!(betting.value.contains("Spend 123 jopacoin"));
        assert!(
            embed
                .fields
                .iter()
                .any(|field| field.name == "🔧 Admin Commands")
        );
    }
    let guild_manager = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request(
                "help",
                777,
                Some(GUILD),
                Some(MANAGE_GUILD_PERMISSION),
                Vec::new(),
            ),
            guild_manager.clone(),
        )
        .await
        .expect("render guild-manager help");
    assert!(
        guild_manager.followups.lock().expect("followups")[0].embeds[0]
            .fields
            .iter()
            .any(|field| field.name == "🔧 Admin Commands")
    );
}

#[tokio::test]
async fn migrated_sqlite_and_live_members_drive_all_six_lazy_tabs() {
    let (_directory, path) = migrated_database();
    seed_leaderboard_data(&path);
    let discord = Arc::new(RecordingDiscord::with_members([
        (ALICE, "AliceLive"),
        (CAROL, "CarolLive"),
    ]));
    let provider = InfoRegistrationProvider::new(path, &config(), discord.clone());
    let registry = registry(&provider);
    let command = registry
        .command_handler("leaderboard")
        .expect("leaderboard handler");
    let responder = Arc::new(RecordingResponder::with_receipt(9_001));
    command
        .handle(
            command_request(
                "leaderboard",
                ALICE,
                Some(GUILD),
                None,
                vec![InteractionOption {
                    name: "limit".to_owned(),
                    value: InteractionValue::Integer(1),
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("render live balance leaderboard");

    assert_eq!(*responder.deferrals.lock().expect("deferrals"), [false]);
    let initial = responder.followups.lock().expect("followups")[0].clone();
    assert!(!initial.ephemeral);
    assert_eq!(
        initial.embeds[0].title.as_deref(),
        Some("LEADERBOARD > Balance")
    );
    let balance_text = initial.embeds[0]
        .description
        .as_deref()
        .expect("balance description");
    assert!(balance_text.contains("<@303>"));
    assert!(!balance_text.contains("<@202>"));
    assert!(balance_text.contains(JOPACOIN_EMOTE));
    assert_eq!(
        initial.allowed_mentions,
        InteractionAllowedMentions::Users(vec![CAROL])
    );
    assert_eq!(initial.components.len(), 2);
    assert_eq!(initial.components[0].buttons.len(), 4);
    assert_eq!(initial.components[1].buttons.len(), 4);
    assert_eq!(initial.components[1].buttons[2].label, " Previous");
    assert_eq!(initial.components[1].buttons[3].label, "Next ");
    assert!(initial.components[1].buttons[2].disabled);
    assert!(initial.components[1].buttons[3].disabled);

    let buttons = initial
        .components
        .iter()
        .flat_map(|row| row.buttons.iter())
        .map(|button| (button.label.clone(), button.custom_id.clone()))
        .collect::<BTreeMap<_, _>>();
    for (index, (label, title)) in [
        ("Gambling", "LEADERBOARD > Gambling"),
        ("Glicko", "LEADERBOARD > Glicko-2 Rating"),
        ("OpenSkill", "LEADERBOARD > OpenSkill Rating"),
        ("Tips", "LEADERBOARD > Tips"),
        ("Trivia", "LEADERBOARD > Trivia"),
    ]
    .into_iter()
    .enumerate()
    {
        let custom_id = &buttons[label];
        registry
            .component_handler(custom_id)
            .expect("info component route")
            .handle(
                component_request(custom_id, 1_000 + index as u64),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("render {label} tab: {error}"));
        assert_eq!(
            responder
                .original_edits
                .lock()
                .expect("original edits")
                .last()
                .and_then(|response| response.embeds[0].title.as_deref()),
            Some(title)
        );
    }

    let loaded_glicko = &buttons["Glicko"];
    let updates_before = responder.updates.lock().expect("updates").len();
    registry
        .component_handler(loaded_glicko)
        .expect("info component route")
        .handle(component_request(loaded_glicko, 9_999), responder.clone())
        .await
        .expect("switch to cached Glicko tab");
    assert_eq!(
        responder.updates.lock().expect("updates").len(),
        updates_before + 1
    );

    let queried = discord.member_lookups.lock().expect("member lookups");
    assert!(queried.contains(&(GUILD, ALICE)));
    assert!(queried.contains(&(GUILD, BOB_DEPARTED)));
    assert!(queried.contains(&(GUILD, CAROL)));
}

#[tokio::test]
async fn leaderboard_candidate_io_failure_uses_python_friendly_recovery() {
    let directory = tempfile::tempdir().expect("temporary invalid info database");
    let path = directory.path().join("missing-schema.db");
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let handler = registry(&provider)
        .command_handler("leaderboard")
        .expect("leaderboard handler");
    let responder = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request("leaderboard", ALICE, Some(GUILD), None, Vec::new()),
            responder.clone(),
        )
        .await
        .expect("friendly recovery response");

    assert_eq!(*responder.deferrals.lock().expect("deferrals"), [false]);
    let response = responder.followups.lock().expect("followups")[0].clone();
    assert_eq!(
        response.content,
        "❌ Couldn't load the leaderboard right now — try again in a moment."
    );
    assert!(response.ephemeral);
    assert_eq!(response.allowed_mentions, InteractionAllowedMentions::None);
}

#[tokio::test]
async fn command_cooldowns_preserve_python_scope_and_response_timing() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let registry = registry(&provider);
    let leaderboard = registry
        .command_handler("leaderboard")
        .expect("leaderboard handler");
    for interaction in 0..3 {
        let responder = Arc::new(RecordingResponder::default());
        leaderboard
            .handle(
                command_request("leaderboard", 55, None, None, Vec::new()),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("allowed leaderboard #{interaction}: {error}"));
        assert_eq!(*responder.deferrals.lock().expect("deferrals"), [false]);
        assert!(responder.responses.lock().expect("responses").is_empty());
    }
    let limited = Arc::new(RecordingResponder::default());
    leaderboard
        .handle(
            command_request("leaderboard", 55, None, None, Vec::new()),
            limited.clone(),
        )
        .await
        .expect("leaderboard cooldown response");
    let response = limited.responses.lock().expect("responses")[0].clone();
    assert!(response.ephemeral);
    assert!(
        response
            .content
            .contains("before using `/leaderboard` again")
    );
    assert!(limited.deferrals.lock().expect("deferrals").is_empty());

    let calibration = registry
        .command_handler("calibration")
        .expect("calibration handler");
    for interaction in 0..2 {
        let responder = Arc::new(RecordingResponder::default());
        calibration
            .handle(
                command_request("calibration", 66, Some(GUILD), None, Vec::new()),
                responder.clone(),
            )
            .await
            .unwrap_or_else(|error| panic!("allowed calibration #{interaction}: {error}"));
        assert_eq!(*responder.deferrals.lock().expect("deferrals"), [true]);
        assert!(responder.responses.lock().expect("responses").is_empty());
    }
    let limited = Arc::new(RecordingResponder::default());
    calibration
        .handle(
            command_request("calibration", 66, Some(GUILD), None, Vec::new()),
            limited.clone(),
        )
        .await
        .expect("calibration cooldown response");
    let response = limited.responses.lock().expect("responses")[0].clone();
    assert!(response.ephemeral);
    assert!(
        response
            .content
            .contains("before using `/calibration` again")
    );
    assert!(limited.deferrals.lock().expect("deferrals").is_empty());
}

#[test]
fn command_cooldown_storage_purges_cold_keys_at_python_threshold() {
    let mut limits = CommandRateLimits::default();
    let cold = Instant::now()
        .checked_sub(Duration::from_secs(31))
        .expect("monotonic clock supports test offset");
    for user_id in 0..1_024 {
        limits
            .hits
            .insert(("cold", 1, user_id), VecDeque::from([cold]));
    }

    assert_eq!(
        limits.retry_after(
            "leaderboard",
            Some(GUILD),
            ALICE,
            3,
            Duration::from_secs(20),
        ),
        None
    );
    assert_eq!(limits.hits.len(), 1);
    assert!(limits.hits.contains_key(&("leaderboard", GUILD, ALICE)));
}

#[tokio::test]
async fn calibration_runs_live_sqlite_analytics_and_emits_real_png_bytes() {
    let (_directory, path) = migrated_database();
    seed_leaderboard_data(&path);
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let handler = registry(&provider)
        .command_handler("calibration")
        .expect("calibration handler");

    let server = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request("calibration", ALICE, Some(GUILD), None, Vec::new()),
            server.clone(),
        )
        .await
        .expect("server calibration");
    assert_eq!(*server.deferrals.lock().expect("deferrals"), [true]);
    let response = server.followups.lock().expect("followups")[0].clone();
    assert!(response.ephemeral);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Rating System Health")
    );
    assert_eq!(
        response.embeds[0].image_url.as_deref(),
        Some("attachment://rating_distribution.png")
    );
    assert_eq!(response.attachments.len(), 1);
    assert_eq!(response.attachments[0].filename, "rating_distribution.png");
    let rating_png = &response.attachments[0].bytes;
    assert!(rating_png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(
        (
            u32::from_be_bytes(rating_png[16..20].try_into().expect("PNG width")),
            u32::from_be_bytes(rating_png[20..24].try_into().expect("PNG height")),
        ),
        (640, 390),
        "the live attachment preserves the reviewed native calibration geometry"
    );
    assert_eq!(
        response.allowed_mentions,
        InteractionAllowedMentions::Users(vec![ALICE, BOB_DEPARTED, CAROL])
    );

    let individual = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request(
                "calibration",
                CAROL,
                Some(GUILD),
                None,
                vec![InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: ALICE,
                        display_name: Some("AliceLive".to_owned()),
                        is_bot: Some(false),
                    },
                }],
            ),
            individual.clone(),
        )
        .await
        .expect("individual calibration");
    let response = individual.followups.lock().expect("followups")[0].clone();
    assert!(response.ephemeral);
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Calibration Stats: AliceLive")
    );
    let profile = response.embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "📊 Rating Profile")
        .expect("rating profile field");
    assert!(profile.value.contains("**Rating:**"));
    assert!(profile.value.contains("**OpenSkill:**"));
    assert_eq!(
        response.allowed_mentions,
        InteractionAllowedMentions::Users(vec![ALICE])
    );
    assert!(response.attachments.is_empty());
}

#[tokio::test]
async fn calibration_with_no_ratings_preserves_the_live_no_attachment_boundary() {
    let (_directory, path) = migrated_database();
    let provider =
        InfoRegistrationProvider::new(path, &config(), Arc::new(RecordingDiscord::default()));
    let handler = registry(&provider)
        .command_handler("calibration")
        .expect("calibration handler");
    let responder = Arc::new(RecordingResponder::default());

    handler
        .handle(
            command_request("calibration", ALICE, Some(GUILD), None, Vec::new()),
            responder.clone(),
        )
        .await
        .expect("empty server calibration");

    let response = responder.followups.lock().expect("followups")[0].clone();
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Rating System Health")
    );
    assert!(response.embeds[0].image_url.is_none());
    assert!(response.attachments.is_empty());
}

#[tokio::test]
async fn individual_calibration_renders_non_default_deployment_rating_config() {
    let (_directory, path) = migrated_database();
    add_player(&path, ALICE, "AliceStored", 10, 1_700.0, 40.0);
    let provider = InfoRegistrationProvider::new(
        path,
        &non_default_calibration_config(),
        Arc::new(RecordingDiscord::default()),
    );
    let handler = registry(&provider)
        .command_handler("calibration")
        .expect("calibration handler");
    let responder = Arc::new(RecordingResponder::default());
    handler
        .handle(
            command_request(
                "calibration",
                CAROL,
                Some(GUILD),
                None,
                vec![InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: ALICE,
                        display_name: Some("AliceLive".to_owned()),
                        is_bot: Some(false),
                    },
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("configured individual calibration");

    let response = responder.followups.lock().expect("followups")[0].clone();
    let drift = response.embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "🎯 Rating Drift")
        .expect("configured rating drift field");
    assert!(drift.value.contains("**+950** rating"));
    let openskill = response.embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "🎲 OpenSkill Rating (Fantasy-Weighted)")
        .expect("configured OpenSkill field");
    assert!(openskill.value.contains("**Calibrated:** No"));
}

#[tokio::test]
async fn view_timeout_drops_session_and_deletes_followup_receipt() {
    let (_directory, path) = migrated_database();
    let provider = InfoRegistrationProvider::with_timeout(
        path,
        &config(),
        Arc::new(RecordingDiscord::default()),
        Duration::from_millis(10),
    );
    let registry = registry(&provider);
    let handler = registry
        .command_handler("leaderboard")
        .expect("leaderboard handler");
    let responder = Arc::new(RecordingResponder::with_receipt(4_242));
    handler
        .handle(
            command_request("leaderboard", 999, None, None, Vec::new()),
            responder.clone(),
        )
        .await
        .expect("leaderboard with expiring view");
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        *responder.deletions.lock().expect("deletions"),
        [InteractionMessageReceipt {
            message_id: 4_242,
            channel_id: CHANNEL,
            delivery: InteractionMessageDelivery::InteractionFollowup,
        }]
    );

    let expired = Arc::new(RecordingResponder::default());
    registry
        .component_handler("info:1:tab:glicko")
        .expect("info component route")
        .handle(component_request("info:1:tab:glicko", 999), expired.clone())
        .await
        .expect("expired component response");
    let response = expired.responses.lock().expect("responses")[0].clone();
    assert!(response.ephemeral);
    assert!(response.content.contains("leaderboard has expired"));
}

#[tokio::test(start_paused = true)]
async fn paging_a_leaderboard_postpones_its_deletion() {
    // discord.py measured a View's timeout from the last interaction, so a
    // leaderboard being actively tabbed must not be deleted mid-use. Time is
    // paused so the assertions are about the timer, not the machine's speed.
    let timeout = Duration::from_secs(840);
    let (_directory, path) = migrated_database();
    let provider = InfoRegistrationProvider::with_timeout(
        path,
        &config(),
        Arc::new(RecordingDiscord::default()),
        timeout,
    );
    let registry = registry(&provider);
    let created = Arc::new(RecordingResponder::with_receipt(4_242));
    registry
        .command_handler("leaderboard")
        .expect("leaderboard handler")
        .handle(
            command_request("leaderboard", 999, None, None, Vec::new()),
            created.clone(),
        )
        .await
        .expect("leaderboard view");

    let component = registry
        .component_handler("info:1:tab:glicko")
        .expect("info component route");
    let responder = Arc::new(RecordingResponder::default());
    for tab in [
        "info:1:tab:glicko",
        "info:1:tab:gambling",
        "info:1:tab:tips",
    ] {
        // Cross most of the current deadline, then use the view again.
        tokio::time::advance(timeout - Duration::from_secs(1)).await;
        component
            .handle(component_request(tab, 999), responder.clone())
            .await
            .expect("switch leaderboard tab");
        tokio::task::yield_now().await;
        assert!(
            created.deletions.lock().expect("deletions").is_empty(),
            "an actively used leaderboard must survive its original timer"
        );
    }

    // Idle past the last interaction's deadline: now it is deleted.
    tokio::time::advance(timeout + Duration::from_secs(1)).await;
    for _ in 0..64 {
        if !created.deletions.lock().expect("deletions").is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        created.deletions.lock().expect("deletions").len(),
        1,
        "the leaderboard is deleted once it goes idle"
    );
}
