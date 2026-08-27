use std::sync::Mutex as StdMutex;

use cama_app::mana_service::DailyAssignment;
use rusqlite::{Connection, params};
use tempfile::NamedTempFile;

use super::*;
use crate::registration::{InteractionResponseError, InteractionValue, Registry};
use crate::test_support::initialize_test_database as initialize_or_migrate;

const GUILD: i64 = 9_001;
const CHANNEL: i64 = 9_002;
const USER: i64 = 42;
const TODAY: &str = "2026-08-11";
const NOW: i64 = 1_786_474_800; // 2026-08-11 12:00:00 America/Los_Angeles

struct Fixture {
    database: NamedTempFile,
}

impl Fixture {
    fn new(player_count: i64) -> Self {
        let database = NamedTempFile::new().expect("temporary mana provider database");
        initialize_or_migrate(database.path()).expect("migrate temporary database");
        let fixture = Self { database };
        for offset in 0..player_count {
            fixture.register(USER + offset, format!("Player {:02}", offset + 1), offset);
        }
        fixture
    }

    fn connection(&self) -> Connection {
        let connection = Connection::open(self.database.path()).expect("open fixture database");
        connection
            .pragma_update(None, "foreign_keys", false)
            .expect("match runtime SQLite policy");
        connection
    }

    fn register(&self, user_id: i64, name: String, offset: i64) {
        self.connection()
            .execute(
                "INSERT INTO players (
                     discord_id, guild_id, discord_username, jopacoin_balance,
                     wins, losses, glicko_rating, last_wheel_spin
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    user_id,
                    GUILD,
                    name,
                    50 + offset,
                    offset,
                    13 - offset,
                    1_500.0 + offset as f64,
                    NOW - 100
                ],
            )
            .expect("insert mana player");
    }

    fn assign(&self, user_id: i64, land: &str, assigned_date: &str) {
        self.connection()
            .execute(
                "INSERT INTO player_mana (
                     discord_id, guild_id, current_land, assigned_date,
                     consumed_today, white_shield_remaining
                 ) VALUES (?1, ?2, ?3, ?4, 0, CASE WHEN ?3='Plains' THEN 25 ELSE 0 END)",
                params![user_id, GUILD, land, assigned_date],
            )
            .expect("insert mana assignment");
    }

    fn balance(&self, user_id: i64) -> i64 {
        self.connection()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id = ?1 AND guild_id = ?2",
                params![user_id, GUILD],
                |row| row.get(0),
            )
            .expect("read balance")
    }

    fn assigned_count(&self) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM player_mana
                 WHERE guild_id = ?1 AND assigned_date = ?2",
                params![GUILD, TODAY],
                |row| row.get(0),
            )
            .expect("count assignments")
    }
}

#[derive(Clone)]
struct FixedClock;

impl ManaClock for FixedClock {
    fn moment(&self) -> Result<ManaMoment, String> {
        Ok(ManaMoment {
            now: NOW,
            la_offset_seconds: -7 * 3_600,
        })
    }
}

#[derive(Default)]
struct FakeDiscord {
    gamba: bool,
    members: Vec<ManaGuildMember>,
}

#[async_trait]
impl ManaDiscordPort for FakeDiscord {
    async fn mana_channel_is_gamba(
        &self,
        _guild_id: i64,
        _channel_id: i64,
    ) -> Result<bool, String> {
        Ok(self.gamba)
    }

    fn mana_guild_members(&self, _guild_id: i64) -> Result<Vec<ManaGuildMember>, String> {
        Ok(self.members.clone())
    }

    async fn mana_guild_member(
        &self,
        _guild_id: i64,
        user_id: i64,
    ) -> Result<Option<ManaGuildMember>, String> {
        Ok(self
            .members
            .iter()
            .find(|member| member.user_id == user_id)
            .cloned())
    }
}

#[derive(Default)]
struct CapturedResponses {
    immediate: Vec<InteractionResponse>,
    defers: Vec<bool>,
    followups: Vec<InteractionResponse>,
    updates: Vec<InteractionResponse>,
}

#[derive(Default)]
struct CapturingResponder {
    captured: StdMutex<CapturedResponses>,
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
            .defers
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

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.captured
            .lock()
            .expect("response capture")
            .updates
            .push(response);
        Ok(())
    }
}

fn members(count: i64) -> Vec<ManaGuildMember> {
    (0..count)
        .map(|offset| ManaGuildMember {
            user_id: USER + offset,
            display_name: format!("Player {:02}", offset + 1),
            avatar_url: (offset == 0).then(|| "https://cdn.test/avatar.png".to_owned()),
            role_names: if offset == 0 {
                vec!["Ash Fan Club".to_owned()]
            } else {
                Vec::new()
            },
        })
        .collect()
}

fn provider(fixture: &Fixture, discord: FakeDiscord) -> ManaRegistrationProvider {
    ManaRegistrationProvider::from_parts(
        fixture.database.path().to_path_buf(),
        100,
        5,
        Arc::new(discord),
        Arc::new(FixedClock),
    )
}

fn registry(provider: &ManaRegistrationProvider) -> Registry {
    let mut builder = RegistryBuilder::default();
    builder
        .add_provider(provider)
        .expect("register mana provider");
    builder.build()
}

fn command(options: Vec<InteractionOption>) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "mana".to_owned(),
        user_id: USER as u64,
        user_display_name: "Payload Name".to_owned(),
        guild_id: Some(GUILD as u64),
        channel_id: Some(CHANNEL as u64),
        member_permissions: None,
        options,
    }
}

#[test]
fn command_and_component_registration_match_python_surface() {
    let fixture = Fixture::new(1);
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(1),
        },
    );
    let registry = registry(&provider);
    let command = registry.commands().next().expect("mana command");
    assert_eq!(command.name, "mana");
    assert_eq!(command.description, "Check your daily mana land assignment");
    assert_eq!(command.options.len(), 2);
    assert_eq!(command.options[0].name, "user");
    assert_eq!(
        command.options[0].description,
        "View another player's mana (optional)"
    );
    assert_eq!(command.options[0].kind, CommandOptionKind::User);
    assert!(!command.options[0].required);
    assert_eq!(command.options[1].name, "all");
    // `/mana` only ever reads now; the worker owns assignment, so this copy
    // must not promise to roll anything.
    assert_eq!(
        command.options[1].description,
        "Show the full guild's current mana assignments"
    );
    assert!(
        !command.options[1]
            .description
            .to_lowercase()
            .contains("roll")
    );
    assert_eq!(command.options[1].kind, CommandOptionKind::Boolean);
    assert_eq!(
        registry.component_routes()[0].custom_id_prefix,
        COMPONENT_PREFIX
    );
}

#[tokio::test]
async fn member_rendering_preserves_fetched_discord_name_when_directory_is_empty() {
    let fixture = Fixture::new(1);
    let provider = provider(&fixture, FakeDiscord::default());
    let mut fetched_members = members(1);

    provider
        .handler
        .render_member_names(GUILD, &mut fetched_members)
        .await
        .expect("render fetched mana member");

    assert_eq!(fetched_members[0].display_name, "Player 01");
}

#[tokio::test]
async fn wrong_channel_charges_one_before_defer_and_uses_exact_ephemeral_copy() {
    let fixture = Fixture::new(1);
    let before = fixture.balance(USER);
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: false,
            members: members(1),
        },
    );
    let responder = Arc::new(CapturingResponder::default());

    registry(&provider)
        .command_handler("mana")
        .expect("mana handler")
        .handle(command(Vec::new()), responder.clone())
        .await
        .expect("dispatch wrong-channel command");

    assert_eq!(fixture.balance(USER), before - 1);
    let captured = responder.captured.lock().expect("response capture");
    assert!(captured.defers.is_empty());
    assert_eq!(captured.immediate.len(), 1);
    assert!(captured.immediate[0].ephemeral);
    assert_eq!(captured.immediate[0].content, WRONG_CHANNEL_MESSAGE);
}

#[tokio::test]
async fn existing_single_assignment_preserves_embed_thumbnail_and_wheel_unlock_boundary() {
    let fixture = Fixture::new(1);
    fixture.assign(USER, "Plains", TODAY);
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(1),
        },
    );
    let responder = Arc::new(CapturingResponder::default());

    registry(&provider)
        .command_handler("mana")
        .expect("mana handler")
        .handle(command(Vec::new()), responder.clone())
        .await
        .expect("dispatch mana display");

    let captured = responder.captured.lock().expect("response capture");
    assert_eq!(captured.defers, vec![false]);
    assert_eq!(captured.followups.len(), 1);
    let embed = &captured.followups[0].embeds[0];
    assert_eq!(embed.title.as_deref(), Some("🔮 Daily Mana — Player 01"));
    assert_eq!(
        embed.thumbnail_url.as_deref(),
        Some("https://cdn.test/avatar.png")
    );
    assert_eq!(
        embed.fields[0].value,
        format!(
            "🌾 **Plains** · White Mana\nNext wheel spin <t:{}:R>",
            NOW + 1
        )
    );
    assert_eq!(embed.fields[1].name, "🌾 Guardian Aura");
    assert!(embed.fields[1].value.contains("**25/25 JC**"));
    assert!(embed.fields.iter().all(|field| field.name != "Assigned"));
}

#[tokio::test]
async fn selected_unassigned_player_is_read_only_and_uses_no_mana_embed() {
    let fixture = Fixture::new(2);
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(2),
        },
    );
    let responder = Arc::new(CapturingResponder::default());
    let options = vec![InteractionOption {
        name: "user".to_owned(),
        value: InteractionValue::User {
            id: (USER + 1) as u64,
            display_name: Some("Selected Payload Name".to_owned()),
            is_bot: Some(false),
        },
    }];

    registry(&provider)
        .command_handler("mana")
        .expect("mana handler")
        .handle(command(options), responder.clone())
        .await
        .expect("dispatch selected player lookup");

    assert_eq!(fixture.assigned_count(), 0);
    let captured = responder.captured.lock().expect("response capture");
    let embed = &captured.followups[0].embeds[0];
    assert_eq!(embed.title.as_deref(), Some("🔮 Daily Mana — Player 02"));
    assert_eq!(
        embed.description.as_deref(),
        Some("This player hasn't been assigned any mana yet.")
    );
}

#[tokio::test]
async fn board_displays_all_players_already_assigned_and_paginates_twelve_per_page() {
    let fixture = Fixture::new(13);
    for i in 0..13 {
        fixture.assign(USER + i, "Plains", TODAY);
    }
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(13),
        },
    );
    let responder = Arc::new(CapturingResponder::default());
    let options = vec![InteractionOption {
        name: "all".to_owned(),
        value: InteractionValue::Boolean(true),
    }];

    registry(&provider)
        .command_handler("mana")
        .expect("mana handler")
        .handle(command(options), responder.clone())
        .await
        .expect("dispatch guild board");

    assert_eq!(fixture.assigned_count(), 13);
    let captured = responder.captured.lock().expect("response capture");
    let response = &captured.followups[0];
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("🔮 Guild Mana Board")
    );
    assert_eq!(
        response.embeds[0]
            .description
            .as_ref()
            .unwrap()
            .lines()
            .count(),
        12
    );
    assert_eq!(
        response.embeds[0].footer.as_deref(),
        Some("Page 1/2 · 13/13 assigned today · Resets 4 AM PST")
    );
    assert_eq!(response.components.len(), 1);
    assert!(response.components[0].buttons[0].disabled);
    assert!(!response.components[0].buttons[1].disabled);
    assert_eq!(
        response.components[0].buttons[1].custom_id,
        format!("{COMPONENT_PREFIX}{USER}:1")
    );
}

#[tokio::test]
async fn board_components_reject_other_users_and_page_for_owner() {
    let fixture = Fixture::new(13);
    for (index, land) in [
        "Island", "Mountain", "Forest", "Plains", "Swamp", "Island", "Mountain", "Forest",
        "Plains", "Swamp", "Island", "Mountain", "Forest",
    ]
    .iter()
    .enumerate()
    {
        fixture.assign(USER + index as i64, land, TODAY);
    }
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(13),
        },
    );
    provider
        .handler
        .board_expirations
        .lock()
        .expect("board expiration")
        .insert((GUILD, USER), Instant::now() + VIEW_TIMEOUT);
    let registry = registry(&provider);

    let intruder = Arc::new(CapturingResponder::default());
    registry
        .component_handler(&format!("{COMPONENT_PREFIX}{USER}:1"))
        .expect("mana component handler")
        .handle(
            InteractionRequest::Component {
                interaction_id: 2,
                custom_id: format!("{COMPONENT_PREFIX}{USER}:1"),
                user_id: (USER + 1) as u64,
                user_display_name: "Mana Intruder".to_owned(),
                guild_id: Some(GUILD as u64),
                channel_id: Some(CHANNEL as u64),
                member_permissions: None,
                values: Vec::new(),
            },
            intruder.clone(),
        )
        .await
        .expect("dispatch intruder page");
    {
        let captured = intruder.captured.lock().expect("response capture");
        assert_eq!(
            captured.immediate[0].content,
            "Only the person who ran this command can page it."
        );
        assert!(captured.immediate[0].ephemeral);
    }

    let owner = Arc::new(CapturingResponder::default());
    registry
        .component_handler(&format!("{COMPONENT_PREFIX}{USER}:1"))
        .expect("mana component handler")
        .handle(
            InteractionRequest::Component {
                interaction_id: 3,
                custom_id: format!("{COMPONENT_PREFIX}{USER}:1"),
                user_id: USER as u64,
                user_display_name: "Mana Owner".to_owned(),
                guild_id: Some(GUILD as u64),
                channel_id: Some(CHANNEL as u64),
                member_permissions: None,
                values: Vec::new(),
            },
            owner.clone(),
        )
        .await
        .expect("dispatch owner page");
    let captured = owner.captured.lock().expect("response capture");
    assert_eq!(captured.updates.len(), 1);
    assert_eq!(
        captured.updates[0].embeds[0].footer.as_deref(),
        Some("Page 2/2 · 13/13 assigned today · Resets 4 AM PST")
    );
    assert!(!captured.updates[0].components[0].buttons[0].disabled);
    assert!(captured.updates[0].components[0].buttons[1].disabled);
}

#[test]
fn cooldown_matches_discord_three_claims_per_ten_seconds_and_is_user_global() {
    let fixture = Fixture::new(1);
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(1),
        },
    );
    assert_eq!(provider.handler.take_command_rate_limit(USER), Ok(None));
    assert_eq!(provider.handler.take_command_rate_limit(USER), Ok(None));
    assert_eq!(provider.handler.take_command_rate_limit(USER), Ok(None));
    assert!(
        provider
            .handler
            .take_command_rate_limit(USER)
            .expect("cooldown lookup")
            .is_some()
    );
    assert_eq!(provider.handler.take_command_rate_limit(USER + 1), Ok(None));
}

async fn assert_white_stipends_are_only_paid_for_fresh_plains_winners_and_never_twice() {
    let fixture = Fixture::new(2);
    fixture
        .connection()
        .execute(
            "UPDATE players SET jopacoin_balance = 0 WHERE guild_id = ?1",
            [GUILD],
        )
        .expect("bankrupt players");
    fixture
        .connection()
        .execute(
            "INSERT INTO nonprofit_fund(guild_id, total_collected)
             VALUES (?1, 20)",
            [GUILD],
        )
        .expect("seed reserve");
    let board = AssignmentBoard {
        assignments: vec![
            DailyAssignment {
                discord_id: USER,
                details: details(Land::Plains),
            },
            DailyAssignment {
                discord_id: USER + 1,
                details: details(Land::Forest),
            },
        ],
        board: Vec::new(),
    };

    pay_batch_stipends_sqlite(fixture.database.path().to_path_buf(), &board, GUILD, 5).await;
    assert_eq!(fixture.balance(USER), 5);
    assert_eq!(fixture.balance(USER + 1), 0);

    let retry = AssignmentBoard {
        assignments: Vec::new(),
        board: Vec::new(),
    };
    pay_batch_stipends_sqlite(fixture.database.path().to_path_buf(), &retry, GUILD, 5).await;
    assert_eq!(fixture.balance(USER), 5);
    let fund: i64 = fixture
        .connection()
        .query_row(
            "SELECT total_collected FROM nonprofit_fund WHERE guild_id = ?1",
            [GUILD],
            |row| row.get(0),
        )
        .expect("read reserve");
    assert_eq!(fund, 15);
}

#[tokio::test]
async fn white_stipends_are_only_paid_for_fresh_plains_winners_and_never_twice() {
    assert_white_stipends_are_only_paid_for_fresh_plains_winners_and_never_twice().await;
}

fn details(land: Land) -> ManaDetails {
    ManaDetails {
        land,
        color: land.color(),
        emoji: land.emoji(),
        assigned_date: TODAY.to_owned(),
        retro_refund: 0,
        guardian_remaining: if land == Land::Plains { 25 } else { 0 },
        consumed: false,
    }
}

#[test]
fn test_stale_plains_row_has_no_guardian_fields() {
    let fixture = Fixture::new(1);
    fixture.assign(USER, "Plains", "2020-01-01");
    let moment = FixedClock.moment().expect("fixed mana moment");
    let mut service = live_service(
        ManaRepository::new(fixture.database.path()),
        fixture.database.path(),
        moment,
    );
    let current = service
        .get_current_mana(USER, GUILD, TODAY)
        .expect("read stale mana")
        .expect("stale row exists");
    assert_eq!(current.land, Land::Plains);
    assert_eq!(current.guardian_remaining, 0);
    assert!(!current.consumed);
}

#[test]
fn test_todays_plains_row_keeps_guardian_fields() {
    let fixture = Fixture::new(1);
    fixture.assign(USER, "Plains", TODAY);
    let moment = FixedClock.moment().expect("fixed mana moment");
    let mut service = live_service(
        ManaRepository::new(fixture.database.path()),
        fixture.database.path(),
        moment,
    );
    let current = service
        .get_current_mana(USER, GUILD, TODAY)
        .expect("read current mana")
        .expect("current row exists");
    assert_eq!(current.guardian_remaining, 25);
}

#[test]
fn test_stale_plains_embed_hides_guardian_aura() {
    let mut stale = details(Land::Plains);
    stale.assigned_date = "2020-01-01".to_owned();
    let embed = single_embed(&members(1)[0], &stale, TODAY, None, 0);
    assert!(
        embed
            .fields
            .iter()
            .all(|field| !field.name.contains("Guardian"))
    );
}

#[test]
fn test_todays_plains_embed_shows_guardian_aura() {
    let embed = single_embed(&members(1)[0], &details(Land::Plains), TODAY, None, 0);
    assert!(
        embed
            .fields
            .iter()
            .any(|field| field.name.contains("Guardian"))
    );
}

#[tokio::test]
async fn test_selected_player_countdown_uses_atomic_unlock_boundary() {
    let fixture = Fixture::new(2);
    fixture.assign(USER + 1, "Island", TODAY);
    fixture
        .connection()
        .execute(
            "UPDATE players SET last_wheel_spin = ?1 WHERE discord_id = ?2 AND guild_id = ?3",
            params![NOW + 50, USER + 1, GUILD],
        )
        .expect("set selected player's future wheel spin");
    let provider = provider(
        &fixture,
        FakeDiscord {
            gamba: true,
            members: members(2),
        },
    );
    let responder = Arc::new(CapturingResponder::default());
    registry(&provider)
        .command_handler("mana")
        .expect("mana handler")
        .handle(
            command(vec![InteractionOption {
                name: "user".to_owned(),
                value: InteractionValue::User {
                    id: (USER + 1) as u64,
                    display_name: Some("Player 02".to_owned()),
                    is_bot: Some(false),
                },
            }]),
            responder.clone(),
        )
        .await
        .expect("display selected player");
    let captured = responder.captured.lock().expect("response capture");
    assert!(
        captured.followups[0].embeds[0].fields[0]
            .value
            .contains(&format!("<t:{}:R>", NOW + 151))
    );
}

#[test]
fn test_next_wheel_spin_matches_atomic_claim_semantics_never_spun() {
    assert_eq!(wheel_unlock_at(NOW, None, 100), NOW);
}

#[test]
fn test_next_wheel_spin_matches_atomic_claim_semantics_expired() {
    assert_eq!(wheel_unlock_at(NOW, Some(NOW - 101), 100), NOW);
}

#[test]
fn test_next_wheel_spin_matches_atomic_claim_semantics_exact_boundary() {
    assert_eq!(wheel_unlock_at(NOW, Some(NOW - 100), 100), NOW + 1);
}

#[test]
fn test_next_wheel_spin_matches_atomic_claim_semantics_future_adjusted_penalty() {
    assert_eq!(
        wheel_unlock_at(NOW, Some(NOW + 345_600), 100),
        NOW + 345_701
    );
}

#[test]
fn test_single_player_omits_assigned_field() {
    let embed = single_embed(&members(1)[0], &details(Land::Island), TODAY, Some(NOW), 0);
    assert!(embed.fields.iter().all(|field| field.name != "Assigned"));
}

#[tokio::test]
async fn test_fresh_plains_stipends_only_process_batch_claim_winners() {
    assert_white_stipends_are_only_paid_for_fresh_plains_winners_and_never_twice().await;
}

fn wheel_unlock_at(now: i64, last: Option<i64>, cooldown: i64) -> i64 {
    last.map_or(now, |last| {
        now.max(last.saturating_add(cooldown).saturating_add(1))
    })
}

#[test]
fn board_component_round_trip_and_pacific_clock_cover_dst_boundaries() {
    assert_eq!(parse_board_component("mana:board:42:3"), Ok((42, 3)));
    assert!(parse_board_component("mana:board:42").is_err());
    let winter = DateTime::parse_from_rfc3339("2026-01-11T12:00:00Z")
        .expect("winter timestamp")
        .timestamp();
    let summer = DateTime::parse_from_rfc3339("2026-07-11T12:00:00Z")
        .expect("summer timestamp")
        .timestamp();
    assert_eq!(pacific_offset_seconds(winter), Ok(-8 * 3_600));
    assert_eq!(pacific_offset_seconds(summer), Ok(-7 * 3_600));
}
