use super::*;
use crate::discord_transport::{
    DiscordAllowedMentions, DiscordEmoji, DiscordGuildMemberSnapshot, DiscordMessageReceipt,
    DiscordMessageSnapshot,
};
use cama_app::service_container::{ServiceContainer, ServiceContainerOptions};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tempfile::NamedTempFile;

type Events = Arc<Mutex<Vec<String>>>;
#[derive(Default)]
struct Responder {
    events: Events,
    responses: Mutex<Vec<InteractionResponse>>,
    defer_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}
impl Responder {
    fn content(&self) -> String {
        self.responses
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.content.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn embed(&self) -> InteractionEmbed {
        self.responses
            .lock()
            .unwrap()
            .iter()
            .flat_map(|response| &response.embeds)
            .next()
            .expect("expected a lobby embed")
            .clone()
    }
}
#[async_trait]
impl InteractionResponder for Responder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.events.lock().unwrap().push("respond".into());
        self.responses.lock().unwrap().push(response);
        Ok(())
    }
    async fn defer(&self, _: bool) -> Result<(), InteractionResponseError> {
        self.events.lock().unwrap().push("defer".into());
        if let Some(hook) = self.defer_hook.lock().unwrap().take() {
            hook();
        }
        Ok(())
    }
    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.events.lock().unwrap().push("followup".into());
        self.responses.lock().unwrap().push(response);
        Ok(())
    }
}
#[derive(Default)]
struct Transport {
    events: Events,
    receipts: Mutex<BTreeMap<(u64, String), DiscordMessageReceipt>>,
    sent: Mutex<Vec<(u64, DiscordMessage)>>,
    parents: Mutex<BTreeMap<u64, u64>>,
    fail_after_send: AtomicBool,
    names: Mutex<BTreeMap<u64, String>>,
}
#[async_trait]
impl DiscordTransport for Transport {
    async fn pin_message(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn archive_thread(&self, _: u64, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    async fn add_reaction(&self, _: u64, _: u64, _: &DiscordEmoji) -> Result<(), String> {
        Ok(())
    }
    async fn remove_reaction(
        &self,
        _: u64,
        _: u64,
        _: &DiscordEmoji,
        _: u64,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn clear_reaction(&self, _: u64, _: u64, _: &DiscordEmoji) -> Result<(), String> {
        Ok(())
    }
    async fn unpin_message(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn send_direct_message(&self, _: u64, _: DiscordMessage) -> Result<(), String> {
        Err("unexpected direct message".into())
    }

    async fn fetch_message(
        &self,
        _: u64,
        _: u64,
    ) -> Result<Option<DiscordMessageSnapshot>, String> {
        Ok(None)
    }
    async fn send_message(
        &self,
        channel: u64,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        let mut sent = self.sent.lock().unwrap();
        let id = 1000 + sent.len() as u64;
        sent.push((channel, message));
        Ok(DiscordMessageReceipt {
            channel_id: channel,
            message_id: id,
            jump_url: format!("https://discord.com/channels/42/{channel}/{id}"),
        })
    }
    async fn send_message_with_delivery_key(
        &self,
        channel: u64,
        key: &str,
        message: DiscordMessage,
    ) -> Result<DiscordMessageReceipt, String> {
        if let Some(receipt) = self
            .receipts
            .lock()
            .unwrap()
            .get(&(channel, key.to_owned()))
            .cloned()
        {
            return Ok(receipt);
        }
        let receipt = self.send_message(channel, message).await?;
        self.receipts
            .lock()
            .unwrap()
            .insert((channel, key.into()), receipt.clone());
        if self.fail_after_send.swap(false, Ordering::SeqCst) {
            return Err("simulated lost send response".into());
        }
        Ok(receipt)
    }
    async fn find_message_by_delivery_key(
        &self,
        channel: u64,
        key: &str,
        _: i64,
        _: usize,
    ) -> Result<Option<DiscordMessageReceipt>, String> {
        Ok(self
            .receipts
            .lock()
            .unwrap()
            .get(&(channel, key.into()))
            .cloned())
    }
    async fn edit_message(&self, _: u64, _: u64, _: DiscordMessage) -> Result<(), String> {
        self.events.lock().unwrap().push("edit".into());
        Ok(())
    }
    async fn delete_message(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn create_public_thread(
        &self,
        channel: u64,
        message: u64,
        _: &str,
    ) -> Result<u64, String> {
        let previous = self.parents.lock().unwrap().insert(message, channel);
        if previous.is_some() {
            Err("already exists".into())
        } else {
            Ok(message)
        }
    }
    async fn guild_member(
        &self,
        _: u64,
        _: u64,
    ) -> Result<Option<DiscordGuildMemberSnapshot>, String> {
        self.events.lock().unwrap().push("member lookup".into());
        Ok(None)
    }
    fn cached_guild_member_render_names(
        &self,
        _: u64,
        _: &[u64],
    ) -> Result<Option<crate::discord_transport::DiscordGuildMemberRenderNames>, String> {
        Ok(Some(self.names.lock().unwrap().clone()))
    }
    async fn channel_parent_id(&self, _: u64, channel: u64) -> Result<Option<u64>, String> {
        self.events.lock().unwrap().push("channel lookup".into());
        Ok(self.parents.lock().unwrap().get(&channel).copied())
    }
}
struct Fixture {
    file: NamedTempFile,
    provider: DeadlockRegistrationProvider,
    transport: Arc<Transport>,
}
impl Fixture {
    fn new() -> Self {
        let file = NamedTempFile::new().unwrap();
        crate::test_support::initialize_test_database(file.path()).unwrap();
        let mut container = ServiceContainer::new(file.path(), ServiceContainerOptions::default());
        container.initialize();
        let app = ApplicationConfig::from_lookup(|key| match key {
            "DISCORD_BOT_TOKEN" => Some("test-token".into()),
            "ADMIN_USER_IDS" => Some("99".into()),
            _ => None,
        })
        .unwrap();
        let config = DeadlockConfig::from_lookup(|key| match key {
            "DEADLOCK_ENABLED" => Some("true".into()),
            "DEADLOCK_CHANNELS" => Some("42:420".into()),
            _ => None,
        })
        .unwrap();
        let transport = Arc::new(Transport::default());
        let provider = DeadlockRegistrationProvider::new(
            file.path(),
            config,
            &app,
            transport.clone(),
            container.components().unwrap().vanity_tax_service.clone(),
            None,
        )
        .unwrap();
        Self {
            file,
            provider,
            transport,
        }
    }
    fn repo(&self) -> DeadlockRepository {
        DeadlockRepository::new(self.file.path())
    }
    fn queue(&self, count: i64, format: DeadlockFormat) {
        for id in 1..=count {
            let seeds = [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard].map(|format| {
                DeadlockSeed {
                    format,
                    mu: 25.0,
                    sigma: 8.333,
                    source: "test prior".into(),
                    source_value: None,
                    provenance: None,
                    source_at: None,
                }
            });
            self.repo()
                .enroll(42, id, id, "Untrusted database display", &seeds, now())
                .unwrap();
            self.repo().queue_join(42, id, now() + id).unwrap();
            self.repo().ready(42, id, format, now()).unwrap();
        }
    }
    fn responder(&self) -> Arc<Responder> {
        Arc::new(Responder {
            events: self.transport.events.clone(),
            ..Responder::default()
        })
    }
    async fn command(
        &self,
        action: &str,
        user: u64,
        channel: u64,
        interaction: u64,
        options: Vec<InteractionOption>,
    ) -> Arc<Responder> {
        let responder = self.responder();
        self.provider
            .handler
            .handle(
                request(action, user, channel, interaction, options),
                responder.clone(),
            )
            .await
            .unwrap();
        responder
    }
    fn game(&self) -> cama_db::deadlock::DeadlockMatch {
        self.repo().recent_matches(42, 1).unwrap().remove(0)
    }
    fn balance(&self, user: i64) -> i64 {
        cama_db::open_runtime_connection(self.file.path())
            .unwrap()
            .query_row(
                "SELECT jopacoin_balance FROM players WHERE guild_id=42 AND discord_id=?1",
                [user],
                |r| r.get(0),
            )
            .unwrap()
    }
}
fn request(
    action: &str,
    user: u64,
    channel: u64,
    interaction: u64,
    options: Vec<InteractionOption>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: interaction,
        name: "deadlock".into(),
        user_id: user,
        user_display_name: format!("Player {user}"),
        guild_id: Some(42),
        channel_id: Some(channel),
        member_permissions: None,
        options: vec![InteractionOption {
            name: action.into(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

#[test]
fn command_contract_has_two_formats_and_no_drafting() {
    let names: Vec<_> = commands().into_iter().map(|c| c.name).collect();
    assert!(names.contains(&"bet".into()));
    assert!(names.contains(&"record".into()));
    assert!(
        !names
            .iter()
            .any(|n| n.contains("draft") || n.contains("captain"))
    );
    assert_eq!(parse_format(&[]).unwrap(), DeadlockFormat::StreetBrawl);
    assert_eq!(
        parse_format(&[text_option("format", "standard")]).unwrap(),
        DeadlockFormat::Standard
    );
    assert!(parse_format(&[text_option("format", "arbitrary")]).is_err());
}

#[tokio::test]
async fn dedicated_channel_is_enforced_after_acknowledgement_before_queue_mutation() {
    let f = Fixture::new();
    f.queue(1, DeadlockFormat::StreetBrawl);
    let reply = f.command("leave", 1, 999, 1, vec![]).await;
    assert!(reply.content().contains("#deadlock-mm"));
    assert_eq!(
        f.repo()
            .queue(42, DeadlockFormat::StreetBrawl)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        &f.transport.events.lock().unwrap()[..2],
        &["defer", "channel lookup"]
    );
    f.transport.parents.lock().unwrap().insert(421, 420);
    let reply = f.command("leave", 1, 421, 2, vec![]).await;
    assert!(!reply.content().contains("Use #deadlock-mm"));
    assert!(
        f.repo()
            .queue(42, DeadlockFormat::StreetBrawl)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn queue_database_read_occurs_after_defer_and_names_fail_closed() {
    let f = Fixture::new();
    let responder = f.responder();
    let repo = f.repo();
    *responder.defer_hook.lock().unwrap() = Some(Box::new(move || {
        let seeds =
            [DeadlockFormat::StreetBrawl, DeadlockFormat::Standard].map(|format| DeadlockSeed {
                format,
                mu: 25.0,
                sigma: 8.333,
                source: "test prior".into(),
                source_value: None,
                provenance: None,
                source_at: None,
            });
        repo.enroll(42, 123456789, 123456789, "<@123456789>", &seeds, now())
            .unwrap();
        repo.queue_join(42, 123456789, now()).unwrap();
    }));
    f.provider
        .handler
        .handle(request("lobby", 1, 420, 1, vec![]), responder.clone())
        .await
        .unwrap();
    let embed = responder.embed();
    let text = format!(
        "{} {}",
        embed.description.as_deref().unwrap(),
        embed
            .fields
            .iter()
            .map(|field| field.value.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(text.contains("1 queued"));
    assert!(text.contains("[Unknown player](https://statlocker.gg/profile/123456789) [25.0]"));
    assert!(!text.contains("[123456789]"));
    assert!(!text.contains("<@123456789>"));
    assert_eq!(f.transport.events.lock().unwrap()[0], "defer");
}

#[tokio::test]
async fn lobby_embed_shows_readable_names_local_mode_ratings_and_ready_status() {
    let f = Fixture::new();
    f.queue(1, DeadlockFormat::StreetBrawl);
    f.transport
        .names
        .lock()
        .unwrap()
        .insert(1, "Ash Chan".into());
    let connection = cama_db::open_runtime_connection(f.file.path()).unwrap();
    connection.execute("UPDATE deadlock_ratings SET mu=31.2 WHERE guild_id=42 AND discord_id=1 AND format='street_brawl'", []).unwrap();
    connection.execute("UPDATE deadlock_ratings SET mu=27.1 WHERE guild_id=42 AND discord_id=1 AND format='standard'", []).unwrap();
    let reply = f.command("lobby", 1, 420, 1, vec![]).await;
    let embed = reply.embed();
    assert_eq!(embed.title.as_deref(), Some("Deadlock · Street Brawl 4v4"));
    assert_eq!(embed.color, Some(0xc69c6d));
    assert!(embed.description.as_deref().unwrap().contains("1/8 ready"));
    assert!(
        embed
            .description
            .as_deref()
            .unwrap()
            .contains("7 more ready players")
    );
    assert_eq!(
        embed.fields[0].value,
        "✓ [Ash Chan](https://statlocker.gg/profile/1) [31.2] · Ready"
    );
    assert!(
        embed
            .footer
            .as_deref()
            .unwrap()
            .contains("Local ratings in brackets")
    );
    {
        let responses = reply.responses.lock().unwrap();
        assert!(responses[0].content.is_empty());
        assert_eq!(responses[0].components[0].buttons.len(), 5);
    }
    let reply = f
        .command("lobby", 1, 420, 2, vec![text_option("format", "standard")])
        .await;
    let embed = reply.embed();
    assert_eq!(embed.title.as_deref(), Some("Deadlock · Standard 6v6"));
    assert!(embed.description.as_deref().unwrap().contains("0/12 ready"));
    assert_eq!(
        embed.fields[0].value,
        "○ [Ash Chan](https://statlocker.gg/profile/1) [27.1] · Waiting"
    );
}

#[tokio::test]
async fn lobby_embed_explains_empty_and_full_ready_states() {
    let f = Fixture::new();
    let empty = f.command("lobby", 1, 420, 1, vec![]).await.embed();
    assert!(empty.description.as_deref().unwrap().contains("0 queued"));
    assert!(empty.fields[0].value.contains("No players yet"));
    f.queue(8, DeadlockFormat::StreetBrawl);
    let ready = f.command("lobby", 1, 420, 2, vec![]).await.embed();
    assert!(ready.description.as_deref().unwrap().contains("8/8 ready"));
    assert!(
        ready
            .description
            .as_deref()
            .unwrap()
            .contains("Ready to shuffle")
    );
    assert!(
        ready
            .fields
            .iter()
            .any(|field| field.name == "Shuffle"
                && field.value.contains("`/shuffle lobby:deadlock`"))
    );
}

#[tokio::test]
async fn lobby_embed_bounds_long_unicode_names_and_reports_queue_overflow() {
    let f = Fixture::new();
    f.queue(25, DeadlockFormat::StreetBrawl);
    for id in 1..=25 {
        f.transport
            .names
            .lock()
            .unwrap()
            .insert(id, format!("Player {}", "🦉".repeat(100)));
    }
    let embed = f.command("lobby", 1, 420, 1, vec![]).await.embed();
    assert!(embed.content_len() <= 6000);
    assert!(
        embed
            .fields
            .iter()
            .all(|field| field.value.encode_utf16().count() <= 1024)
    );
    let players: Vec<_> = embed
        .fields
        .iter()
        .filter(|field| field.name.starts_with("Players"))
        .collect();
    assert!(players.len() > 1);
    assert_eq!(
        players
            .iter()
            .map(|field| field.value.matches("[25.0]").count())
            .sum::<usize>(),
        20
    );
    assert!(
        embed.fields.iter().any(
            |field| field.name == "Also queued" && field.value == "5 more players are waiting."
        )
    );
}

fn imported_rating_fixture() -> ImportedRatings {
    let standard = crate::deadlock_ratings::ImportedRating {
        mu: 36.0,
        sigma: 8.333,
        source: "valve-rank-weak-prior-v1".into(),
        raw_value: Some(95.0),
        provenance: serde_json::json!({"provider": "deadlock-api", "source_mode": "ranked"}),
    };
    let mut brawl = standard.clone();
    brawl.mu = 27.75;
    brawl.source = "standard-weak-brawl-prior-v1".into();
    ImportedRatings { standard, brawl }
}

#[tokio::test]
async fn registration_shows_ratings_and_fallback_reason_without_external_api_links() {
    let f = Fixture::new();
    f.provider.handler.ratings.seed_cache(
        1,
        ImportedRatings::provisional("No usable current ranked badge"),
    );
    let reply = f
        .command("register", 1, 420, 1, vec![text_option("steam", "1")])
        .await;
    let text = reply.content();
    assert!(text.contains("Standard **25.0** · Brawl **25.0**"));
    assert!(text.contains("No external rating was available; starting with the default rating."));
    assert!(!text.contains("API key"));
    assert!(!text.contains("Import status:"));
    assert!(!text.contains("deadlock-api"));
    assert!(!text.contains("Deadlock API"));
    assert!(text.contains("[Statlocker profile](https://statlocker.gg/profile/1)"));
    assert_eq!(f.transport.events.lock().unwrap()[0], "defer");
    let retry = f
        .command("register", 1, 420, 2, vec![text_option("steam", "1")])
        .await;
    assert!(
        retry
            .content()
            .contains("Your local ratings were preserved")
    );
    assert!(
        retry
            .content()
            .contains("No external rating was available; starting with the default rating.")
    );
}

#[tokio::test]
async fn registration_retries_only_unplayed_neutral_priors_without_another_wallet_grant() {
    let f = Fixture::new();
    f.provider.handler.ratings.seed_cache(
        1,
        ImportedRatings::provisional("No usable current ranked badge"),
    );
    f.command("register", 1, 420, 1, vec![text_option("steam", "1")])
        .await;
    let balance = f.balance(1);
    // Standard already has a local result; only the still-unplayed Brawl seed can change.
    cama_db::open_runtime_connection(f.file.path()).unwrap().execute("UPDATE deadlock_ratings SET mu=29.3,games=1,revision=1 WHERE guild_id=42 AND discord_id=1 AND format='standard'", []).unwrap();
    f.provider
        .handler
        .ratings
        .seed_cache(1, imported_rating_fixture());
    let reply = f
        .command("register", 1, 420, 2, vec![text_option("steam", "1")])
        .await;
    let text = reply.content();
    assert!(text.contains("Registration refreshed"));
    assert!(text.contains("Standard **29.3** · Brawl **27.8**"));
    assert!(text.contains("Initial rating: Valve ranked badge (provisional)."));
    assert!(text.contains("[Statlocker profile](https://statlocker.gg/profile/1)"));
    assert!(!text.contains("deadlock-api"));
    assert_eq!(f.balance(1), balance);
    let ratings = f.repo().ratings(42, 1).unwrap();
    assert!(
        ratings
            .iter()
            .any(|rating| rating.format == DeadlockFormat::Standard
                && rating.mu == 29.3
                && rating.games == 1)
    );
    assert!(
        ratings
            .iter()
            .any(|rating| rating.format == DeadlockFormat::StreetBrawl
                && rating.mu == 27.75
                && rating.games == 0)
    );
    let reply = f
        .command("register", 1, 420, 3, vec![text_option("steam", "1")])
        .await;
    assert!(
        reply
            .content()
            .contains("Your local ratings were preserved")
    );
    assert_eq!(f.balance(1), balance);
}

#[tokio::test]
async fn registration_converts_steam64_to_unsigned_account_id_for_profile_links() {
    let f = Fixture::new();
    let account = u32::MAX;
    f.provider
        .handler
        .ratings
        .seed_cache(account, imported_rating_fixture());
    let steam64 = (76_561_197_960_265_728_u64 + u64::from(account)).to_string();
    let reply = f
        .command("register", 1, 420, 1, vec![text_option("steam", &steam64)])
        .await;
    let text = reply.content();
    assert!(text.contains("[Statlocker profile](https://statlocker.gg/profile/4294967295)"));
    assert!(!text.contains(&steam64));
    assert!(!text.contains("deadlock-api"));
    assert_eq!(
        f.repo().enrolled(42, 1).unwrap().unwrap().steam_id,
        i64::from(account)
    );
}

#[tokio::test]
async fn match_profile_links_escape_names_and_fit_discord_with_twelve_long_names() {
    let f = Fixture::new();
    f.queue(12, DeadlockFormat::Standard);
    for id in 1..=12 {
        f.transport
            .names
            .lock()
            .unwrap()
            .insert(id, "[🦉*_](example)".repeat(20));
    }
    f.command(
        "shuffle",
        99,
        420,
        1,
        vec![text_option("format", "standard")],
    )
    .await;
    let reply = f
        .command(
            "match",
            99,
            420,
            2,
            vec![int_option("match", f.game().match_id)],
        )
        .await;
    let text = reply.content();
    assert!(text.encode_utf16().count() <= 2000);
    assert_eq!(text.matches("https://statlocker.gg/profile/").count(), 12);
    assert!(text.contains("\\[🦉\\*\\_\\]"));
    assert!(!text.contains("deadlock-api"));
}

#[tokio::test]
async fn standard_requires_twelve_ready_players_without_consuming_brawl_queue() {
    let f = Fixture::new();
    f.queue(8, DeadlockFormat::StreetBrawl);
    let reply = f
        .command(
            "shuffle",
            99,
            420,
            1,
            vec![text_option("format", "standard")],
        )
        .await;
    assert!(reply.content().contains("12 ready players required"));
    assert_eq!(
        f.repo()
            .queue(42, DeadlockFormat::StreetBrawl)
            .unwrap()
            .len(),
        8
    );
    assert!(f.repo().recent_matches(42, 10).unwrap().is_empty());
    let reply = f.command("shuffle", 99, 420, 2, vec![]).await;
    assert!(reply.content().contains("Deadlock #"));
    let game = f.game();
    assert_eq!(game.format, DeadlockFormat::StreetBrawl);
    assert_eq!(game.team1.len(), 4);
    assert!(game.publication_thread_id.is_some());
}

#[tokio::test]
async fn standard_override_is_one_shuffle_and_uses_six_players_per_side() {
    let f = Fixture::new();
    f.queue(12, DeadlockFormat::Standard);
    f.command(
        "shuffle",
        99,
        420,
        1,
        vec![text_option("format", "standard")],
    )
    .await;
    let game = f.game();
    assert_eq!(game.format, DeadlockFormat::Standard);
    assert_eq!(game.team1.len(), 6);
    assert_eq!(game.team2.len(), 6);
    assert_eq!(parse_format(&[]).unwrap(), DeadlockFormat::StreetBrawl);
}

#[tokio::test]
async fn ambiguous_publication_recovers_without_duplicate_match_messages_or_wagers() {
    let f = Fixture::new();
    f.queue(8, DeadlockFormat::StreetBrawl);
    f.transport.fail_after_send.store(true, Ordering::SeqCst);
    let reply = f.command("shuffle", 99, 420, 1, vec![]).await;
    assert!(reply.content().contains("saved"));
    let game = f.game();
    assert!(game.publication_message_id.is_none());
    let thread = f
        .provider
        .handler
        .publish_match(42, game.match_id)
        .await
        .unwrap();
    f.provider
        .handler
        .publish_match(42, game.match_id)
        .await
        .unwrap();
    assert_eq!(f.repo().recent_matches(42, 10).unwrap().len(), 1);
    assert_eq!(f.transport.sent.lock().unwrap().len(), 3);
    assert_eq!(f.game().publication_thread_id, Some(thread as i64));
    assert!(
        f.transport
            .sent
            .lock()
            .unwrap()
            .iter()
            .all(|(_, message)| message.allowed_mentions == DiscordAllowedMentions::None)
    );
    let connection = cama_db::open_runtime_connection(f.file.path()).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM deadlock_betting_markets", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn participant_bets_cannot_target_opponents_and_record_requires_organizer() {
    let f = Fixture::new();
    f.queue(8, DeadlockFormat::StreetBrawl);
    f.command("shuffle", 99, 420, 1, vec![]).await;
    let game = f.game();
    let player = game.team1[0].discord_id;
    let connection = cama_db::open_runtime_connection(f.file.path()).unwrap();
    connection
        .execute(
            "INSERT INTO pending_matches(pending_match_id,guild_id,payload) VALUES(?1,42,'{}')",
            [game.match_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100 WHERE guild_id=42",
            [],
        )
        .unwrap();
    let reply = f
        .command(
            "bet",
            player as u64,
            420,
            2,
            vec![
                int_option("match", game.match_id),
                text_option("team", "team2"),
                int_option("amount", 5),
            ],
        )
        .await;
    assert!(!reply.content().contains("JC debited"));
    assert_eq!(f.balance(player), 100);
    let reply = f
        .command(
            "bet",
            player as u64,
            420,
            3,
            vec![
                int_option("match", game.match_id),
                text_option("team", "team1"),
                int_option("amount", 5),
            ],
        )
        .await;
    assert!(reply.content().contains("5 JC debited"));
    assert_eq!(f.balance(player), 95);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM bets", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let reply = f
        .command(
            "record",
            player as u64,
            420,
            4,
            vec![
                int_option("match", game.match_id),
                text_option("winner", "team1"),
            ],
        )
        .await;
    assert!(reply.content().contains("organizer or an admin"));
    assert_eq!(f.game().winner, None);
    let market = DeadlockBettingRepository::new(f.file.path())
        .market(42, game.match_id)
        .unwrap()
        .unwrap();
    assert_eq!(market.status, "open");
}

#[tokio::test]
async fn nonadmin_exact_shuffle_retry_reuses_match_after_leaving_queue() {
    let fixture = Fixture::new();
    fixture.queue(8, DeadlockFormat::StreetBrawl);
    let first = fixture.command("shuffle", 1, 420, 901, vec![]).await;
    assert!(first.content().contains("Deadlock #"));
    let original = fixture.game();
    assert!(
        fixture
            .repo()
            .queue(42, DeadlockFormat::StreetBrawl)
            .unwrap()
            .is_empty()
    );
    let retry = fixture.command("shuffle", 1, 420, 901, vec![]).await;
    assert!(
        retry
            .content()
            .contains(&format!("Deadlock #{}", original.match_id)),
        "{}",
        retry.content()
    );
    assert_eq!(fixture.repo().recent_matches(42, 10).unwrap().len(), 1);
    assert_eq!(fixture.transport.sent.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn organizer_must_close_then_records_once_and_conflicting_result_cannot_pay_again() {
    let f = Fixture::new();
    f.queue(8, DeadlockFormat::StreetBrawl);
    f.command("shuffle", 1, 420, 100, vec![]).await;
    let game = f.game();
    let winner = game.team1[0].discord_id;
    let loser = game.team2[0].discord_id;
    let connection = cama_db::open_runtime_connection(f.file.path()).unwrap();
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100 WHERE guild_id=42",
            [],
        )
        .unwrap();
    for (user, team, amount, interaction) in [(winner, "team1", 10, 101), (loser, "team2", 30, 102)]
    {
        let reply = f
            .command(
                "bet",
                user as u64,
                420,
                interaction,
                vec![
                    int_option("match", game.match_id),
                    text_option("team", team),
                    int_option("amount", amount),
                ],
            )
            .await;
        assert!(
            reply.content().contains("JC debited"),
            "{}",
            reply.content()
        );
    }
    assert_eq!((f.balance(winner), f.balance(loser)), (90, 70));
    let result_options = || {
        vec![
            int_option("match", game.match_id),
            text_option("winner", "team1"),
        ]
    };
    let open_reply = f.command("record", 1, 420, 103, result_options()).await;
    assert!(open_reply.content().contains("close the betting market"));
    assert_eq!(f.game().winner, None);
    assert_eq!((f.balance(winner), f.balance(loser)), (90, 70));

    let closed = f
        .command(
            "close",
            1,
            420,
            104,
            vec![int_option("match", game.match_id)],
        )
        .await;
    assert!(closed.content().contains("is closed"));
    let recorded = f.command("record", 1, 420, 105, result_options()).await;
    assert!(
        recorded.content().contains("Deadlock #"),
        "{}",
        recorded.content()
    );
    assert_eq!(f.game().status, "settled");
    assert_eq!(f.game().winner, Some(1));
    assert_eq!((f.balance(winner), f.balance(loser)), (130, 70));

    let retry = f.command("record", 1, 420, 106, result_options()).await;
    assert!(retry.content().contains("Deadlock #"));
    assert_eq!((f.balance(winner), f.balance(loser)), (130, 70));
    let conflict = f
        .command(
            "record",
            1,
            420,
            107,
            vec![
                int_option("match", game.match_id),
                text_option("winner", "team2"),
            ],
        )
        .await;
    assert!(conflict.content().contains("conflicting result"));
    assert_eq!(f.game().winner, Some(1));
    assert_eq!((f.balance(winner), f.balance(loser)), (130, 70));
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM deadlock_economic_receipts WHERE market_id=?1 AND kind='settlement'",
        [format!("deadlock:{}", game.match_id)], |r| r.get::<_, i64>(0),
    ).unwrap(), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM deadlock_rating_events WHERE match_id=?1",
                [game.match_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        8
    );
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM economy_ledger_entries WHERE related_id=?1 AND source='deadlock_payout'",
        [format!("deadlock:{}", game.match_id)], |r| r.get::<_, i64>(0),
    ).unwrap(), 1);
}

#[tokio::test]
async fn organizer_abort_retry_refunds_effective_stakes_once_without_rating_a_match() {
    let f = Fixture::new();
    f.queue(8, DeadlockFormat::StreetBrawl);
    f.command("shuffle", 1, 420, 200, vec![]).await;
    let game = f.game();
    let first = game.team1[0].discord_id;
    let second = game.team2[0].discord_id;
    let connection = cama_db::open_runtime_connection(f.file.path()).unwrap();
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=100 WHERE guild_id=42",
            [],
        )
        .unwrap();
    for (user, team, amount, leverage, interaction) in
        [(first, "team1", 10, 2, 201), (second, "team2", 15, 1, 202)]
    {
        let reply = f
            .command(
                "bet",
                user as u64,
                420,
                interaction,
                vec![
                    int_option("match", game.match_id),
                    text_option("team", team),
                    int_option("amount", amount),
                    int_option("leverage", leverage),
                ],
            )
            .await;
        assert!(
            reply.content().contains("JC debited"),
            "{}",
            reply.content()
        );
    }
    assert_eq!((f.balance(first), f.balance(second)), (80, 85));
    for interaction in [203, 204] {
        let reply = f
            .command(
                "abort",
                1,
                420,
                interaction,
                vec![
                    int_option("match", game.match_id),
                    text_option("reason", "Player disconnected before launch"),
                ],
            )
            .await;
        assert!(
            reply.content().contains("Deadlock #"),
            "{}",
            reply.content()
        );
        assert_eq!(f.game().status, "aborted");
        assert_eq!(f.game().winner, None);
        assert_eq!((f.balance(first), f.balance(second)), (100, 100));
    }
    let market = DeadlockBettingRepository::new(f.file.path())
        .market(42, game.match_id)
        .unwrap()
        .unwrap();
    assert_eq!(market.status, "refunded");
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM deadlock_economic_receipts WHERE market_id=?1 AND kind='refund'",
        [format!("deadlock:{}", game.match_id)], |r| r.get::<_, i64>(0),
    ).unwrap(), 1);
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM economy_ledger_entries WHERE related_id=?1 AND source='deadlock_refund'",
        [format!("deadlock:{}", game.match_id)], |r| r.get::<_, i64>(0),
    ).unwrap(), 2);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM deadlock_rating_events WHERE match_id=?1",
                [game.match_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}
