use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use crate::test_support::initialize_test_database as initialize_or_migrate;
use async_trait::async_trait;
use cama_app::dig_media_runtime::DigMediaRuntime;
use cama_app::dig_runtime::{DEFAULT_DIG_ASSET_ROOT, DigRuntimeService};
use cama_app::dig_runtime::{DigRuntimeConfig, DigRuntimeOutcome};
use cama_app::service_container::{ServiceContainer, ServiceContainerOptions, VanityMember};
use cama_db::core_repositories::{NewPlayer, PlayerRepository};
use rusqlite::{Connection, params};
use tempfile::{NamedTempFile, tempdir};

use super::dig_options;
use super::{
    DigAbandonViewAdmission, DigBonusDispatchPort, DigBossNeonVictory, DigChannelSnapshot,
    DigDiscordPort, DigEventPendingDeliveryQuery, DigPrestigeViewAdmission, DigPublicHistory,
    DigPublicHistoryMessage, DigPublicSendFailure, DigRegistrationProvider,
    DigRuntimeBloodPactSnapshot, DigRuntimeFlavorSnapshot, JOPACOIN_EMOTE,
};
use crate::application_config::ApplicationConfig;
use crate::discord_transport::{GuildPlayerNameDirectory, GuildPlayerNameResolver};
use crate::gateway_events::{GatewayMember, GuildMemberPageSource, ReadyRecoveryContext};
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, InteractionHandler,
    InteractionMessageReceipt, InteractionModal, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionResponseError, InteractionValue,
    RegistrationProvider,
};

const USER: u64 = 77_001;
const GUILD: u64 = 77_002;
const CHANNEL: u64 = 77_003;

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

#[test]
fn prestige_picker_embeds_match_python_presentation() {
    use cama_app::dig_prestige_runtime::{
        DigPrestigePreview, PrestigeMutationChoice, PrestigeMutationPreview, PrestigePerkChoice,
    };

    let mutation_choice = PrestigeMutationChoice {
        id: "bright".to_owned(),
        name: "Bright Vein".to_owned(),
        description: "More treasure, more danger.".to_owned(),
        positive: true,
    };
    let preview = DigPrestigePreview {
        can_prestige: true,
        reason: None,
        current_level: 7,
        target_level: 8,
        run_score: 4_200,
        available_perks: vec!["steady_hands".to_owned()],
        offered_perks: vec![
            PrestigePerkChoice {
                id: "steady_hands".to_owned(),
                name: "Steady Hands".to_owned(),
            },
            PrestigePerkChoice {
                id: "deep_pockets".to_owned(),
                name: "Deep Pockets".to_owned(),
            },
        ],
        mutation: Some(PrestigeMutationPreview {
            forced: PrestigeMutationChoice {
                id: "brittle".to_owned(),
                name: "Brittle Stone".to_owned(),
                description: "Cave-ins strike harder.".to_owned(),
                positive: false,
            },
            choices: vec![mutation_choice],
        }),
        ascension_unlock: None,
    };

    let mutation = super::prestige_mutation_response(&preview, "token");
    let mutation_embed = &mutation.embeds[0];
    assert_eq!(mutation_embed.title.as_deref(), Some("Prestige to P8?"));
    assert!(mutation_embed.fields.iter().any(|field| {
        field.name == "Choose a Mutation"
            && field.value == "**Bright Vein** — More treasure, more danger."
    }));

    let initial_perks = super::prestige_perk_response(&preview, "token", None);
    let initial_embed = &initial_perks.embeds[0];
    assert_eq!(initial_embed.title.as_deref(), Some("Prestige to P8?"));
    assert!(initial_embed.fields.iter().any(|field| {
        field.name == "Choose a Perk" && field.value == "**Steady Hands**\n**Deep Pockets**"
    }));

    let post_mutation = super::prestige_perk_response(&preview, "token", Some("bright"));
    let post_mutation_embed = &post_mutation.embeds[0];
    assert_eq!(
        post_mutation_embed.title.as_deref(),
        Some("Choose a Prestige Perk")
    );
    assert_eq!(
        post_mutation_embed.description.as_deref(),
        Some("**Steady Hands**\n**Deep Pockets**")
    );
    assert!(post_mutation_embed.fields.is_empty());
}

fn persistent_vanity_tax(
    database_path: impl AsRef<std::path::Path>,
) -> Arc<cama_app::service_container::PersistentVanityTaxService> {
    persistent_vanity_tax_with_rate(database_path, 0.10)
}

fn persistent_vanity_tax_with_rate(
    database_path: impl AsRef<std::path::Path>,
    rate: f64,
) -> Arc<cama_app::service_container::PersistentVanityTaxService> {
    let options = ServiceContainerOptions {
        vanity_tax_rate: rate,
        ..ServiceContainerOptions::default()
    };
    let mut container = ServiceContainer::new(database_path, options);
    container.initialize();
    Arc::clone(
        &container
            .components()
            .expect("vanity-tax service components")
            .vanity_tax_service,
    )
}

#[test]
fn production_constructor_owns_media_and_fails_closed_on_schema_admission() {
    let admitted = NamedTempFile::new().expect("admitted provider database");
    initialize_or_migrate(admitted.path()).expect("admit canonical schema");
    let vanity_tax = persistent_vanity_tax(admitted.path());
    let provider = DigRegistrationProvider::production(
        admitted.path(),
        &config(),
        Arc::clone(&vanity_tax),
        Arc::new(TestDiscord::default()),
        None,
        None,
        Arc::new(RecordingBonusDispatcher::default()),
    )
    .expect("production constructor admits canonical schema");
    let mut registry = crate::registration::RegistryBuilder::default();
    provider
        .register(&mut registry)
        .expect("production provider registers");

    let missing_directory = tempdir().expect("missing schema directory");
    let result = DigRegistrationProvider::production(
        missing_directory.path().join("not-created.db"),
        &config(),
        vanity_tax,
        Arc::new(TestDiscord::default()),
        None,
        None,
        Arc::new(RecordingBonusDispatcher::default()),
    );
    assert!(matches!(
        result,
        Err(super::DigProviderBuildError::Database(message))
            if message.contains("does not exist") || message.contains("schema")
    ));
}

#[test]
fn boss_reminder_predicate_requires_a_committed_resolution() {
    let pending = cama_app::dig_boss_runtime::DigBossRuntimeResult {
        outcome: (),
        action_id: None,
        abandoned_action_id: None,
        abandoned_wager_forfeit: 0,
        abandoned_gear_wear: Default::default(),
        gear_drop: None,
        prestige_relic_drop: None,
        broken_gear: Vec::new(),
        warnings: Vec::new(),
        notices: Vec::new(),
    };
    assert!(!super::boss_resume_is_resolved(&pending));

    let committed = cama_app::dig_boss_runtime::DigBossRuntimeResult {
        action_id: Some(44),
        ..pending
    };
    assert!(super::boss_resume_is_resolved(&committed));
}

fn boss_call_result<T>(
    outcome: T,
    action_id: Option<i64>,
) -> cama_app::dig_boss_runtime::DigBossRuntimeResult<T> {
    cama_app::dig_boss_runtime::DigBossRuntimeResult {
        outcome,
        action_id,
        abandoned_action_id: None,
        abandoned_wager_forfeit: 0,
        abandoned_gear_wear: Default::default(),
        gear_drop: None,
        prestige_relic_drop: None,
        broken_gear: Vec::new(),
        warnings: Vec::new(),
        notices: Vec::new(),
    }
}

fn pinnacle_boss_projection_fixture(
    won: bool,
    phase: u8,
    next_phase: u8,
) -> cama_app::dig_boss_runtime::DigPinnacleResolved {
    cama_app::dig_boss_runtime::DigPinnacleResolved {
        won,
        boss_id: "forgotten_king".to_owned(),
        boss_name: "The Crowned Hunger".to_owned(),
        phase,
        next_phase,
        risk_tier: cama_app::boss_duel::RiskTier::Bold,
        wager: 10,
        win_chance: 0.60,
        jc_delta: if won { 500 } else { -10 },
        payout: if won { 500 } else { 0 },
        wager_payout: if won { 500 } else { 0 },
        gross_jc: 500,
        scaled_base_jc: 500,
        reward_multiplier: 1.0,
        gross_payout: if won { 500 } else { 0 },
        bankruptcy_penalty: 0,
        vanity_tax: 0,
        low_priority_tax: 0,
        new_depth: if won { 350 } else { 340 },
        boss_hp_remaining: if won { 0 } else { 50 },
        boss_hp_max: 100,
        starting_boss_hp: 100,
        knockback: if won { 0 } else { 10 },
        round_log: Vec::new(),
        gear_wear: Default::default(),
        phase_event_id: None,
        relic_drop: None,
        rescue_line_used: false,
        warding_salts_blocked: false,
    }
}

#[test]
fn boss_neon_start_and_resume_emit_only_terminal_regular_wins() {
    let terminal = regular_boss_projection_fixture(true);
    let start = super::boss_start_neon_victory(&boss_call_result(
        cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
            terminal.clone(),
        )),
        Some(101),
    ))
    .expect("terminal start victory");
    assert_eq!(start.boss_name, "Grothak the Unbreakable");
    assert_eq!(start.boundary, 100);
    assert_eq!(start.jc_delta, terminal.jc_delta);

    let resume = super::boss_resume_neon_victory(&boss_call_result(
        cama_app::dig_boss_runtime::DigBossResolvedOutcome::Regular(Box::new(terminal)),
        Some(102),
    ));
    assert!(
        resume.is_some(),
        "legacy and namespaced resume use the same gate"
    );

    let mut phase_only = regular_boss_projection_fixture(true);
    phase_only.phase_transition = Some(cama_app::dig_bosses::BossStatus::PhaseOneDefeated);
    assert!(
        super::boss_start_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
                phase_only.clone(),
            )),
            Some(103),
        ))
        .is_none()
    );
    assert!(
        super::boss_resume_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossResolvedOutcome::Regular(Box::new(phase_only)),
            Some(104),
        ))
        .is_none()
    );

    let loss = regular_boss_projection_fixture(false);
    assert!(
        super::boss_start_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(loss)),
            Some(105),
        ))
        .is_none()
    );
    assert!(
        super::boss_resume_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossResolvedOutcome::Regular(Box::new(
                regular_boss_projection_fixture(false),
            )),
            Some(106),
        ))
        .is_none()
    );
    assert!(
        super::boss_start_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
                regular_boss_projection_fixture(true),
            )),
            None,
        ))
        .is_none(),
        "uncommitted outcomes cannot emit"
    );
}

#[test]
fn boss_neon_prestige_relic_does_not_masquerade_as_trophy_boost() {
    let mut result = boss_call_result(
        cama_app::dig_boss_runtime::DigBossStartOutcome::RegularResolved(Box::new(
            regular_boss_projection_fixture(true),
        )),
        Some(107),
    );
    result.prestige_relic_drop = Some(cama_app::dig_boss_runtime::DigBossPrestigeRelicDrop {
        database_id: 7,
        artifact_id: "pinnacle_relic".to_owned(),
        name: "Pinnacle Relic".to_owned(),
        rarity: "legendary".to_owned(),
    });
    let victory = super::boss_start_neon_victory(&result).expect("terminal victory");
    assert!(!victory.trophy_relic_drop);
}

#[test]
fn boss_neon_pinnacle_gate_selects_terminal_mode_only() {
    let terminal = pinnacle_boss_projection_fixture(true, 3, 0);
    let start = super::boss_start_neon_victory(&boss_call_result(
        cama_app::dig_boss_runtime::DigBossStartOutcome::PinnacleResolved(Box::new(
            terminal.clone(),
        )),
        Some(201),
    ))
    .expect("terminal Pinnacle start victory");
    assert_eq!(
        start.boundary,
        i64::from(cama_app::dig_bosses::PINNACLE_DEPTH)
    );
    assert_eq!(start.boss_name, "The Crowned Hunger");

    let resume = super::boss_resume_neon_victory(&boss_call_result(
        cama_app::dig_boss_runtime::DigBossResolvedOutcome::Pinnacle(Box::new(terminal)),
        Some(202),
    ));
    assert!(
        resume.is_some(),
        "terminal Pinnacle resume uses pinnacle(false)"
    );

    assert!(
        super::boss_start_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossStartOutcome::PinnacleResolved(Box::new(
                pinnacle_boss_projection_fixture(true, 2, 3),
            )),
            Some(203),
        ))
        .is_none(),
        "phase-only Pinnacle progress must not emit"
    );
    assert!(
        super::boss_resume_neon_victory(&boss_call_result(
            cama_app::dig_boss_runtime::DigBossResolvedOutcome::Pinnacle(Box::new(
                pinnacle_boss_projection_fixture(false, 3, 3),
            )),
            Some(204),
        ))
        .is_none(),
        "Pinnacle losses must not emit"
    );
}

// tests/test_dig_reminder_command.py::test_run_dig_reuses_registration_check_and_embedded_notice
#[tokio::test]
async fn run_dig_returns_the_registered_typed_execution_and_delivery_notice() {
    let (_database, provider, _discord) = fixture();
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0x1234,
                CHANNEL as i64,
                "Dig Test Miner",
                None,
            ),
        )
        .await
        .expect("registered dig execution");
    assert!(execution.outcome.success);
    let delivery = execution.delivery.expect("immutable delivery notice");
    assert_eq!(delivery.context.channel_id, CHANNEL as i64);
    assert_eq!(delivery.context.display_name, "Dig Test Miner");
    assert!(!delivery.render.description.is_empty());
}

#[tokio::test]
async fn durable_dig_delivery_keeps_stored_username_while_rendering_server_nickname() {
    let (database, provider, _discord) = fixture();
    let provider =
        provider.with_player_names(Arc::new(FixedPlayerNames(GuildPlayerNameDirectory::new(
            Some(BTreeMap::from([(USER, Some("Server Miner".to_owned()))])),
        ))));
    let first = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0x5677,
                CHANNEL as i64,
                "dig-test-miner",
                None,
            ),
        )
        .await
        .expect("first registered dig execution");
    assert_eq!(
        first
            .delivery
            .expect("first durable delivery snapshot")
            .context
            .display_name,
        "dig-test-miner"
    );
    Connection::open(database.path())
        .expect("open Dig database")
        .execute(
            "UPDATE tunnels SET last_dig_at=0 WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
        )
        .expect("reset Dig cooldown");
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0x5678,
                CHANNEL as i64,
                "dig-test-miner",
                Some("https://example.invalid/miner.png".to_owned()),
            ),
        )
        .await
        .expect("second registered dig execution");
    let delivery = execution.delivery.expect("durable delivery snapshot");

    assert_eq!(delivery.context.display_name, "dig-test-miner");
    let (main, _event) = provider.handler.render_delivery_responses(&delivery);
    assert_eq!(main.embeds[0].author_name.as_deref(), Some("Server Miner"));
    assert_eq!(delivery.context.display_name, "dig-test-miner");
}

#[tokio::test]
async fn production_runtime_injects_shared_vanity_tax_into_live_dig() {
    let (database, base, discord) = fixture();
    let now = 1_900_210_029;
    let connection = Connection::open(database.path()).expect("vanity provider database");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at,luminosity,
              boss_progress)
             VALUES (?1,?2,276,276,1,?3,100,?4)",
            params![
                USER as i64,
                GUILD as i64,
                now - 7_200,
                r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"},"100":{"status":"defeated"},"150":{"status":"defeated"},"200":{"status":"defeated"},"275":{"status":"defeated"}}"#,
            ],
        )
        .expect("normal Dig tunnel");
    let game_date =
        cama_domain::game_date::game_date_for_timestamp(now as f64).expect("vanity-tax game date");
    connection
        .execute(
            "INSERT INTO dig_weather(guild_id,game_date,layer_name,weather_id)
             VALUES (?1,?2,'The Hollow','mineral_vein'),
                    (?1,?2,'Dirt','earthworm_migration')",
            params![GUILD as i64, game_date],
        )
        .expect("deterministic vanity-tax weather");
    drop(connection);
    let vanity_tax = persistent_vanity_tax_with_rate(database.path(), 1.0);
    vanity_tax
        .refresh_guild(
            GUILD as i64,
            &[VanityMember {
                discord_id: USER as i64,
                nickname: None,
            }],
        )
        .expect("refresh taxable Dig member");
    let provider = DigRegistrationProvider::with_media_ai_and_vanity(
        database.path(),
        &config(),
        discord,
        None,
        Arc::clone(&base.handler.state.media),
        None,
        Some(vanity_tax),
        0,
    )
    .expect("provider with shared vanity tax");
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            now,
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0xdec0_de01,
                CHANNEL as i64,
                "Taxed Dig Miner",
                None,
            ),
        )
        .await
        .expect("taxed normal Dig");
    assert!(execution.outcome.success);
    assert!(!execution.outcome.cave_in);
    assert!(
        execution.outcome.vanity_tax > 0,
        "production Dig outcome was not taxed: {:?}",
        execution.outcome
    );
    let connection = Connection::open(database.path()).expect("inspect vanity-tax ledger");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM economy_ledger_entries
                 WHERE account_id=?1 AND guild_id=?2 AND source='vanity_tax'",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("provider vanity-tax ledger count"),
        1,
    );
}

#[tokio::test]
async fn delivery_settles_blood_pact_before_flavor_and_projects_net_copy() {
    let (database, provider, _discord) = fixture();
    let now = 1_900_210_029;
    let skimmer = 77_004_i64;
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            skimmer,
            "blood-pact-holder",
            Some(GUILD as i64),
        ))
        .expect("Blood Pact holder");
    let connection = Connection::open(database.path()).expect("Blood Pact provider database");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,total_digs,last_dig_at,luminosity,
              boss_progress)
             VALUES (?1,?2,76,76,1,?3,100,?4)",
            params![
                USER as i64,
                GUILD as i64,
                now - 7_200,
                r#"{"25":{"status":"defeated"},"50":{"status":"defeated"},"75":{"status":"defeated"}}"#,
            ],
        )
        .expect("Blood Pact normal Dig tunnel");
    connection
        .execute(
            "INSERT INTO manashop_buffs(
                 discord_id,guild_id,buff_type,target_id,granted_at,expires_at,
                 triggered,data
             ) VALUES(?1,?2,'blood_pact',?3,?4,?5,0,?6)",
            params![
                skimmer,
                GUILD as i64,
                USER as i64,
                now - 1,
                now + 86_400,
                serde_json::json!({
                    "skimmed_total": 0,
                    "cap": 100,
                    "skim_rate": 1.0,
                })
                .to_string(),
            ],
        )
        .expect("active Blood Pact");
    drop(connection);

    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            now,
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0xdec0_de02,
                CHANNEL as i64,
                "Pacted Dig Miner",
                None,
            ),
        )
        .await
        .expect("committed Blood Pact Dig");
    let gross = execution.outcome.jc_earned;
    assert!(gross > 0);
    let pending = execution.delivery.expect("pending Blood Pact delivery");
    assert_eq!(pending.blood_pact, DigRuntimeBloodPactSnapshot::Pending);

    let settled = provider
        .handler
        .prepare_delivery(&pending)
        .await
        .expect("settle Blood Pact before flavor");
    let skimmed = match settled.blood_pact {
        DigRuntimeBloodPactSnapshot::Applied { skimmed } => skimmed,
        ref other => panic!("expected applied Blood Pact, got {other:?}"),
    };
    assert!(skimmed > 0);
    assert!(settled.flavor.is_terminal());
    assert_eq!(
        settled.outcome.jc_earned, gross,
        "immutable outcome changed"
    );
    let projected = super::dig_blood_pact_projected_outcome(&settled);
    assert_eq!(projected.jc_earned, gross - skimmed);
    assert_eq!(
        projected.balance_after,
        settled.outcome.balance_after - skimmed
    );
    let (response, event) = super::dig_delivery_responses(
        &settled,
        &provider.handler.state.media,
        &provider.handler.state.view_nonce,
    );
    assert!(event.is_none());
    let progress = response.embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "Progress")
        .map(|field| field.value.as_str())
        .expect("Blood Pact delivery progress");
    assert!(progress.contains(&format!("+{} {JOPACOIN_EMOTE}", gross - skimmed)));
    assert!(!progress.contains(&format!("+{gross} {JOPACOIN_EMOTE}")));
    let connection = Connection::open(database.path()).expect("inspect Blood Pact delivery");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM hostile_loss_events
                 WHERE guild_id=?1 AND victim_id=?2",
                params![GUILD as i64, USER as i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("Blood Pact hostile-loss event count"),
        1,
    );
}

// tests/test_dig_reminder_command.py::test_schedule_dig_reminder_is_noop_without_service
#[tokio::test]
async fn schedule_dig_reminder_is_a_noop_without_a_reminder_service() {
    let (_database, provider, _discord) = fixture();
    assert!(provider.handler.state.reminder_hooks.is_none());
    provider
        .handler
        .reconcile_dig_reminder(USER as i64, GUILD as i64, super::unix_now())
        .await;
}

// tests/test_dig_reminder_command.py::test_reconciliation_failure_does_not_interrupt_dig_and_warns
#[tokio::test]
async fn failed_reminder_reconciliation_does_not_interrupt_a_committed_dig() {
    let (database, base, _discord) = fixture();
    let invalid_store = tempdir().expect("invalid reminder store directory");
    let reminder = crate::reminder_provider::tests::test_provider(
        invalid_store.path(),
        Arc::new(crate::reminder_provider::tests::MockDiscord::default()),
        Default::default(),
    );
    let provider = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        Arc::new(TestDiscord::default()),
        Some(reminder.hooks()),
        Arc::clone(&base.handler.state.media),
    );
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0x1235,
                CHANNEL as i64,
                "Dig Test Miner",
                None,
            ),
        )
        .await
        .expect("dig remains successful when reminder storage fails");
    assert!(execution.outcome.success);
    provider
        .handler
        .reconcile_dig_reminder(USER as i64, GUILD as i64, super::unix_now())
        .await;
}

// tests/test_dig_reminder_command.py::test_boss_encounter_receives_reminder_callback
#[tokio::test]
async fn resolved_boss_reconciles_the_dig_reminder_callback() {
    let (database, base, _discord) = fixture();
    let now = super::unix_now();
    Connection::open(database.path())
        .expect("boss callback database")
        .execute(
            "INSERT INTO tunnels (discord_id,guild_id,last_dig_at)
             VALUES (?1,?2,?3)",
            rusqlite::params![USER as i64, GUILD as i64, now],
        )
        .expect("seed boss callback tunnel");
    Connection::open(database.path())
        .expect("boss reminder preferences database")
        .execute(
            "INSERT INTO reminder_preferences
                (discord_id,guild_id,dig_enabled,updated_at)
             VALUES (?1,?2,1,?3)",
            rusqlite::params![USER as i64, GUILD as i64, now],
        )
        .expect("enable boss callback reminder");
    let reminder = crate::reminder_provider::tests::test_provider(
        database.path(),
        Arc::new(crate::reminder_provider::tests::MockDiscord::default()),
        Default::default(),
    );
    let provider = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        Arc::new(TestDiscord::default()),
        Some(reminder.hooks()),
        Arc::clone(&base.handler.state.media),
    );
    provider
        .handler
        .reconcile_resolved_boss(USER as i64, GUILD as i64, now)
        .await;
    let key = cama_app::reminders::ReminderTaskKey::new(
        cama_app::reminders::UserId::new(USER as i64),
        cama_app::reminders::GuildId::new(GUILD as i64),
        cama_app::reminders::ReminderKind::Dig,
    );
    let task = reminder
        .hooks()
        .test_task_snapshot(key)
        .expect("boss callback reminder snapshot")
        .expect("boss callback schedules reminder");
    assert_eq!(
        task.due_at,
        now + cama_app::dig_service::FREE_DIG_COOLDOWN_SECONDS
    );
}

// tests/test_dig_reminder_command.py::test_boss_encounter_embed_surfaces_carried_wager
#[test]
fn boss_encounter_embed_surfaces_the_carried_wager() {
    let (_database, provider, _discord) = fixture();
    let info = cama_app::dig_boss_runtime::DigBossEncounterInfo {
        boundary: 350,
        boss_id: "forgotten_king".to_owned(),
        boss_name: "The Crowned Hunger".to_owned(),
        dialogue: "The crown burns.".to_owned(),
        is_pinnacle: true,
        phase: 2,
        wager_allowed: true,
        carried_wager: Some(1_500),
        carried_risk_tier: None,
        has_scout_lantern: false,
        luminosity: 100,
        encounter_key: "carried-wager".to_owned(),
    };
    let response = super::boss_encounter_response(
        &info,
        USER as i64,
        GUILD as i64,
        &provider.handler.state.media,
    );
    let field = response.embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "Carried Wager")
        .expect("carried wager field");
    assert_eq!(
        field.value,
        format!("**1,500** {JOPACOIN_EMOTE} is already riding on this phase.")
    );
    assert!(!field.value.contains("**1,500** JC "));
}

// tests/test_dig_reminder_command.py::test_resumed_pinnacle_encounter_uses_phase_presentation[2]
#[test]
fn resumed_pinnacle_phase_two_uses_the_normal_presentation() {
    let (_database, provider, _discord) = fixture();
    let info = cama_app::dig_boss_runtime::DigBossEncounterInfo {
        boundary: 350,
        boss_id: "forgotten_king".to_owned(),
        boss_name: "The Crowned Hunger".to_owned(),
        dialogue: "The crown burns.".to_owned(),
        is_pinnacle: true,
        phase: 2,
        wager_allowed: true,
        carried_wager: None,
        carried_risk_tier: None,
        has_scout_lantern: false,
        luminosity: 100,
        encounter_key: "phase-two".to_owned(),
    };
    let response = super::boss_encounter_response(
        &info,
        USER as i64,
        GUILD as i64,
        &provider.handler.state.media,
    );
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Boss Encountered: The Crowned Hunger!")
    );
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some("The crown burns.")
    );
}

// tests/test_dig_reminder_command.py::test_resumed_pinnacle_encounter_uses_phase_presentation[3]
#[test]
fn resumed_pinnacle_phase_three_uses_the_secret_presentation() {
    let (_database, provider, _discord) = fixture();
    let info = cama_app::dig_boss_runtime::DigBossEncounterInfo {
        boundary: 350,
        boss_id: "forgotten_king".to_owned(),
        boss_name: "The Last Breath of Kings".to_owned(),
        dialogue: "Last breath.".to_owned(),
        is_pinnacle: true,
        phase: 3,
        wager_allowed: true,
        carried_wager: None,
        carried_risk_tier: None,
        has_scout_lantern: false,
        luminosity: 100,
        encounter_key: "phase-three".to_owned(),
    };
    let response = super::boss_encounter_response_with_roll(
        &info,
        USER as i64,
        GUILD as i64,
        &provider.handler.state.media,
        Some(0.0),
    );
    assert_eq!(
        response.embeds[0].title.as_deref(),
        Some("Boss Encountered: The Crown Remembers!")
    );
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some("A king's last room opens behind the throne.")
    );
}

fn regular_boss_projection_fixture(won: bool) -> cama_app::boss_multi_tier::ResolvedFight {
    cama_app::boss_multi_tier::ResolvedFight {
        won,
        boss_id: "grothak".to_owned(),
        boundary: 100,
        wager: 10,
        win_chance: 0.60,
        jc_delta: if won { 500 } else { -10 },
        gross_payout: if won { 500 } else { 0 },
        bankruptcy_penalty: 0,
        vanity_tax: 0,
        low_priority_tax: 0,
        new_depth: if won { 100 } else { 95 },
        boss_hp_remaining: if won { 0 } else { 50 },
        boss_hp_max: 100,
        starting_boss_hp: 100,
        extra_knockback: 0,
        extra_cooldown_seconds: 0,
        round_log: Vec::new(),
        gear_wear: cama_app::boss_multi_tier::GearWearResult::default(),
        phase_transition: None,
        boss_preparation: None,
        rescue_line_used: false,
        warding_salts_blocked: false,
    }
}

#[test]
fn boss_mechanic_prompt_shows_authored_copy_choices_and_live_hp() {
    use cama_app::boss_duel::RiskTier;
    use cama_app::boss_multi_tier::{
        CombatEffects, CombatState, GearSnapshot, PausedBossDuel, PendingPrompt, mechanic_by_id,
    };

    let mechanic = mechanic_by_id("ogre_fireblast").expect("Ogre mechanic is bundled");
    let paused = PausedBossDuel {
        boss_id: "ogre_magi".to_owned(),
        boundary: 75,
        mechanic_id: mechanic.id.clone(),
        risk_tier: RiskTier::Bold,
        wager: 0,
        combat: CombatState {
            player_hp: 7,
            boss_hp: 11,
            player_hit: 0.5,
            player_damage: 2,
            boss_hit: 0.5,
            boss_damage: 2,
            critical_chance: 0.0,
            critical_bonus: 0,
            effects: CombatEffects::default(),
        },
        round_num: 2,
        round_log: Vec::new(),
        pending_prompt: PendingPrompt::from(&mechanic),
        attempts_this_fight: 1,
        initial_win_chance: 0.5,
        payout_multiplier: 1.0,
        player_hp_max: 10,
        boss_hp_max: 14,
        starting_boss_hp: 14,
        gear_snapshot: GearSnapshot::default(),
        forced_no_wager_phase: false,
        echo_applied: false,
        echo_killer_id: None,
    };

    let response = super::paused_boss_response(&paused, USER as i64, GUILD as i64);
    let embed = &response.embeds[0];
    assert_eq!(embed.title.as_deref(), Some("The Twin-Skulled — Round 2"));
    assert_eq!(
        embed.description.as_deref(),
        Some(
            "**The Twin-Skulled chants a slow fire blast**\n*Left head counts down. Right head forgot the number.*"
        )
    );
    assert_eq!(
        boss_projection_field(embed, "State").map(|field| field.value.as_str()),
        Some("You: **7/10 HP**  |  The Twin-Skulled: **11/14 HP**")
    );
    assert_eq!(
        boss_projection_field(embed, "Your choice").map(|field| field.value.as_str()),
        Some("1. Slap the left head\n2. Confuse both heads\n3. Stand in front and grin")
    );
}

#[test]
fn boss_result_shows_selected_outcome_and_softened_hp() {
    use cama_app::boss_multi_tier::{MechanicRoundRecord, RoundEffectLog, RoundRecord};

    let mut result = regular_boss_projection_fixture(false);
    result.boss_hp_remaining = 40;
    result.round_log.push(RoundRecord {
        round: 2,
        player_hp: 4,
        boss_hp: 40,
        player_hit: false,
        boss_hit: None,
        pet_assist: None,
        pet_assist_damage: 0,
        effect_log: RoundEffectLog::default(),
        mechanic: Some(MechanicRoundRecord {
            mechanic_id: "ogre_fireblast".to_owned(),
            option_index: 2,
            option_label: "Stand in front and grin".to_owned(),
            narrative: "Right head casts backwards. Ogre lights himself up.".to_owned(),
            warding_salts_blocked: false,
        }),
    });

    let embed = super::regular_boss_result_embed(&result, None, None, &[]);
    assert_eq!(
        boss_projection_field(&embed, "You chose: Stand in front and grin")
            .map(|field| field.value.as_str()),
        Some("Right head casts backwards. Ogre lights himself up.")
    );
    assert_eq!(
        boss_projection_field(&embed, "The boss remembers").map(|field| field.value.as_str()),
        Some(
            "You knocked Grothak the Unbreakable from **100/100 HP** to **40/100 HP** before retreating."
        )
    );
}

fn boss_projection_field<'a>(
    embed: &'a crate::registration::InteractionEmbed,
    name: &str,
) -> Option<&'a crate::registration::InteractionEmbedField> {
    embed.fields.iter().find(|field| field.name == name)
}

// tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_no_drop_omits_field
#[test]
fn regular_boss_victory_without_drop_omits_both_loud_drop_fields() {
    let resolved = regular_boss_projection_fixture(true);
    let embed = super::regular_boss_result_embed(&resolved, None, None, &[]);

    assert!(boss_projection_field(&embed, "Boss Drop").is_none());
    assert!(boss_projection_field(&embed, "Relic Found").is_none());
}

// tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_gear_drop_renders_field
#[test]
fn regular_boss_victory_projects_gear_drop_name_and_slot() {
    let resolved = regular_boss_projection_fixture(true);
    let drop = cama_app::dig_boss_runtime::DigBossGearDrop {
        gear_id: 1,
        slot: cama_domain::dig_gear::GearSlot::Weapon,
        tier: 6,
        name: "Void-Touched Pickaxe".to_owned(),
    };
    let embed = super::regular_boss_result_embed(&resolved, Some(&drop), None, &[]);
    let field = boss_projection_field(&embed, "Boss Drop").expect("boss drop field");

    assert!(field.value.contains("Void-Touched Pickaxe"));
    assert!(field.value.contains("weapon"));
}

// tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_prestige_relic_drop_renders_field
#[test]
fn regular_boss_victory_projects_prestige_relic_name() {
    let resolved = regular_boss_projection_fixture(true);
    let drop = cama_app::dig_boss_runtime::DigBossPrestigeRelicDrop {
        database_id: 2,
        artifact_id: "echo_stone".to_owned(),
        name: "Echo Stone".to_owned(),
        rarity: "Rare".to_owned(),
    };
    let embed = super::regular_boss_result_embed(&resolved, None, Some(&drop), &[]);
    let field = boss_projection_field(&embed, "Relic Found").expect("relic field");

    assert!(field.value.contains("Echo Stone"));
}

// tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_loss_does_not_render_drops
#[test]
fn regular_boss_loss_suppresses_loud_drop_fields() {
    let resolved = regular_boss_projection_fixture(false);
    let gear = cama_app::dig_boss_runtime::DigBossGearDrop {
        gear_id: 1,
        slot: cama_domain::dig_gear::GearSlot::Weapon,
        tier: 1,
        name: "Should not appear".to_owned(),
    };
    let relic = cama_app::dig_boss_runtime::DigBossPrestigeRelicDrop {
        database_id: 2,
        artifact_id: "also_hidden".to_owned(),
        name: "Also Hidden".to_owned(),
        rarity: "Rare".to_owned(),
    };
    let embed = super::regular_boss_result_embed(&resolved, Some(&gear), Some(&relic), &[]);

    assert!(boss_projection_field(&embed, "Boss Drop").is_none());
    assert!(boss_projection_field(&embed, "Relic Found").is_none());
}

// tests/test_dig_loud_drops.py::TestBossFightResultEmbedDrops::test_broken_gear_renders_repair_notification
#[test]
fn regular_boss_broken_gear_projects_name_disabled_effects_and_repair_all() {
    let resolved = regular_boss_projection_fixture(true);
    let broken = [cama_app::dig_boss_runtime::DigBossBrokenGear {
        gear_id: 3,
        name: "Ironclad Armor".to_owned(),
    }];
    let embed = super::regular_boss_result_embed(&resolved, None, None, &broken);
    let field = boss_projection_field(&embed, "Gear Broken").expect("broken gear field");

    assert!(field.value.contains("Ironclad Armor"));
    assert!(field.value.contains("effects disabled"));
    assert!(field.value.contains("Repair All"));
}

#[test]
fn boss_failure_uses_python_error_embed() {
    let response = super::boss_error_response("The guardian refuses the wager.");
    assert!(response.content.is_empty());
    let embed = &response.embeds[0];
    assert_eq!(embed.title.as_deref(), Some("Boss Fight Error"));
    assert_eq!(
        embed.description.as_deref(),
        Some("The guardian refuses the wager.")
    );
    assert_eq!(embed.color, Some(0xFF_A5_00));
}

#[tokio::test]
async fn stale_namespaced_boss_duel_is_reported_to_the_player() {
    let (_database, provider, _discord) = fixture();
    let responder = Arc::new(TestResponder::default());

    provider
        .handler
        .handle(
            component_request(format!("dig:boss:duel:{USER}:{GUILD}:0"), Vec::new()),
            responder.clone(),
        )
        .await
        .expect("a stale boss choice is a handled interaction");

    assert_eq!(*responder.defers.lock().expect("boss defers"), vec![false]);
    let followups = responder.followups.lock().expect("boss followups");
    assert_eq!(followups.len(), 1);
    let embed = &followups[0].embeds[0];
    assert_eq!(embed.title.as_deref(), Some("Boss Fight Error"));
    assert_eq!(
        embed.description.as_deref(),
        Some("there is no active duel to resume")
    );
}

#[tokio::test]
async fn stale_carried_boss_fight_is_reported_as_expired() {
    let (database, provider, _discord) = fixture();
    Connection::open(database.path())
        .expect("stale boss fixture DB")
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
              boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
             VALUES (?1,?2,1,1,0,'{}',0,0,100,0,'Stale Boss Button')",
            params![USER as i64, GUILD as i64],
        )
        .expect("non-boss tunnel");
    let responder = Arc::new(TestResponder::default());

    provider
        .handler
        .handle(
            component_request(format!("dig:boss:fight:carried:{USER}:{GUILD}"), Vec::new()),
            responder.clone(),
        )
        .await
        .expect("a stale boss fight is a handled interaction");

    assert_eq!(*responder.defers.lock().expect("boss defers"), vec![false]);
    assert!(
        responder
            .responses
            .lock()
            .expect("boss responses")
            .is_empty()
    );
    let followups = responder.followups.lock().expect("boss followups");
    assert_eq!(followups.len(), 1);
    assert!(followups[0].ephemeral);
    assert_eq!(
        followups[0].content,
        "This boss encounter is no longer active. Use `/dig go` to continue."
    );
}

// tests/test_dig_event_messaging.py::test_event_result_embed_surfaces_gear_drop_details
#[test]
fn failed_event_uses_python_error_embed() {
    let outcome = cama_app::dig_event_runtime::DigEventRuntimeOutcome {
        success: false,
        error: Some("The tunnel rejects that choice.".to_owned()),
        resolution: None,
        depth_before: 12,
        depth_after: 12,
        balance_after: 50,
        action_id: Some(17),
        reward_row_id: None,
        applied_now: false,
        splash: None,
        guild_modifier: None,
        chain_event: None,
        quest_finale: None,
        retryable: false,
    };

    let response = super::event_resolution_response(&outcome);
    assert!(response.ephemeral);
    assert!(response.content.is_empty());
    let embed = &response.embeds[0];
    assert_eq!(embed.title.as_deref(), Some("Event Failed"));
    assert_eq!(
        embed.description.as_deref(),
        Some("The tunnel rejects that choice.")
    );
    assert_eq!(embed.color, Some(0xFF_44_44));
}

// tests/test_dig_event_messaging.py::test_event_result_embed_surfaces_gear_drop_details
#[test]
fn event_result_embed_surfaces_exact_unique_gear_drop_details() {
    let outcome = cama_app::dig_event_runtime::DigEventRuntimeOutcome {
        success: true,
        error: None,
        resolution: Some(cama_app::dig_loot::CanonicalEventResolution {
            event_id: "collapsed_armory".to_owned(),
            event_name: "Collapsed Armory".to_owned(),
            choice: "risky".to_owned(),
            complexity: cama_app::dig_loot::EventComplexity::Choice,
            descriptions: Vec::new(),
            steps: Vec::new(),
            boon_options: Vec::new(),
            ascii_art: None,
            social: false,
            succeeded: true,
            message: "The armory coughs up one last bad idea.".to_owned(),
            advance: 0,
            jc: 4,
            cave_in: false,
            streak_loss: 0,
            streak_days_after: None,
            curse: None,
            persisted_curse: None,
            buff: None,
            black_wax_seal_spent: false,
            active_curse_remaining_after: None,
            curse_cleared: false,
            gear_reward_pool: vec!["glassbreaker_pick".to_owned()],
            consumable_reward_pool: Vec::new(),
            artifact_reward_pool: Vec::new(),
            splash: None,
            guild_modifier_on_success: None,
            quest_id: None,
            quest_step: None,
            next_event_id: None,
            chained_event_id: None,
            reward: Some(cama_app::dig_loot::CanonicalReward::Gear(
                "glassbreaker_pick".to_owned(),
            )),
            duplicate_gear_reward: false,
            splash_payout_ratio: None,
            economy_gross_jc: 4,
            cruel_echoes: false,
            boss_encounter: false,
            random_plan: cama_app::dig_loot::CanonicalEventRandomPlan::default(),
        }),
        depth_before: 175,
        depth_after: 175,
        balance_after: 504,
        action_id: Some(17),
        reward_row_id: Some(23),
        applied_now: true,
        splash: None,
        guild_modifier: None,
        chain_event: None,
        quest_finale: None,
        retryable: false,
    };

    let response = super::event_resolution_response(&outcome);
    let field = response.embeds[0]
        .fields
        .iter()
        .find(|field| field.name == "Gear Drop")
        .expect("gear drop field");
    assert!(field.value.contains("Glassbreaker Pick"));
    assert!(field.value.contains("Weapon"));
    assert!(field.value.contains("Durability: 8"));
    assert!(
        field
            .value
            .contains("Diamond dig bonuses; +2 boss damage; -8% hit chance.")
    );
    assert!(field.value.contains("Stored in your gear inventory"));
    assert!(field.value.contains("`/dig gear`"));
}

#[test]
fn rust_deployment_retains_authored_dig_assets_at_the_runtime_root() {
    const DOCKERFILE: &str = include_str!("../../../../../Dockerfile.rust");
    const COPY_CONTRACT: &str = "COPY --chown=appuser:appuser assets/dig/ /app/assets/dig/";

    assert_eq!(DEFAULT_DIG_ASSET_ROOT, "/app/assets/dig");
    assert!(
        DOCKERFILE.lines().any(|line| line.trim() == COPY_CONTRACT),
        "Dockerfile.rust must retain the authored Dig tree at {DEFAULT_DIG_ASSET_ROOT}"
    );
}

struct TestDiscord {
    public: StdMutex<Vec<InteractionResponse>>,
    temporary: StdMutex<Vec<(i64, Duration, InteractionResponse)>>,
    reactions: StdMutex<Vec<(i64, u64, String)>>,
    public_history: StdMutex<Vec<DigPublicHistoryMessage>>,
    message_channels: StdMutex<BTreeMap<u64, i64>>,
    available_channels: StdMutex<BTreeSet<i64>>,
    accept_then_fail_nonce_send: StdMutex<bool>,
    reject_next_configured_nonce_send: StdMutex<bool>,
    fail_next_history: StdMutex<bool>,
    reject_un_nonnced_public_send: StdMutex<bool>,
    gamba: bool,
    avatar_url: Option<String>,
    lifecycle: Option<Arc<StdMutex<Vec<&'static str>>>>,
}

impl Default for TestDiscord {
    fn default() -> Self {
        Self {
            public: StdMutex::new(Vec::new()),
            temporary: StdMutex::new(Vec::new()),
            reactions: StdMutex::new(Vec::new()),
            public_history: StdMutex::new(Vec::new()),
            message_channels: StdMutex::new(BTreeMap::new()),
            available_channels: StdMutex::new(BTreeSet::new()),
            accept_then_fail_nonce_send: StdMutex::new(false),
            reject_next_configured_nonce_send: StdMutex::new(false),
            fail_next_history: StdMutex::new(false),
            reject_un_nonnced_public_send: StdMutex::new(false),
            gamba: true,
            avatar_url: None,
            lifecycle: None,
        }
    }
}

impl TestDiscord {
    fn with_channels(channels: impl IntoIterator<Item = i64>) -> Self {
        let discord = Self::default();
        discord
            .available_channels
            .lock()
            .expect("available channels")
            .extend(channels);
        discord
    }

    fn arm_accept_then_fail_nonce_send(&self) {
        *self
            .accept_then_fail_nonce_send
            .lock()
            .expect("nonce send fault") = true;
    }

    fn reject_next_configured_nonce_send(&self) {
        *self
            .reject_next_configured_nonce_send
            .lock()
            .expect("configured nonce send fault") = true;
    }

    fn reject_un_nonnced_public_send(&self) {
        *self
            .reject_un_nonnced_public_send
            .lock()
            .expect("un-nonced send fault") = true;
    }

    fn allow_un_nonnced_public_send(&self) {
        *self
            .reject_un_nonnced_public_send
            .lock()
            .expect("un-nonced send fault") = false;
    }

    fn with_lifecycle(mut self, lifecycle: Arc<StdMutex<Vec<&'static str>>>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }
}

#[async_trait]
impl DigDiscordPort for TestDiscord {
    async fn dig_channel(&self, channel_id: i64) -> Result<Option<DigChannelSnapshot>, String> {
        Ok(self
            .available_channels
            .lock()
            .expect("available channels")
            .contains(&channel_id)
            .then_some(DigChannelSnapshot {
                id: channel_id,
                guild_id: Some(GUILD as i64),
                parent_id: (channel_id == CHANNEL as i64).then_some(CHANNEL as i64 + 1),
                can_send: true,
                is_text: true,
            }))
    }

    async fn dig_channel_is_gamba(&self, _guild_id: i64, _channel_id: i64) -> Result<bool, String> {
        Ok(self.gamba)
    }

    async fn dig_user_avatar_url(
        &self,
        _guild_id: i64,
        _user_id: i64,
    ) -> Result<Option<String>, String> {
        Ok(self.avatar_url.clone())
    }

    async fn dig_send_public(
        &self,
        _channel_id: i64,
        response: InteractionResponse,
    ) -> Result<(), String> {
        if *self
            .reject_un_nonnced_public_send
            .lock()
            .expect("un-nonced send fault")
        {
            return Err("test forbids un-nonced public sends".to_owned());
        }
        self.public.lock().expect("public responses").push(response);
        Ok(())
    }

    async fn dig_send_temporary(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        delete_after: Duration,
    ) -> Result<(), String> {
        let result = self.dig_send_public(channel_id, response.clone()).await;
        if result.is_ok() {
            if let Some(lifecycle) = &self.lifecycle {
                lifecycle.lock().expect("lifecycle log").push("temporary");
            }
            self.temporary.lock().expect("temporary responses").push((
                channel_id,
                delete_after,
                response,
            ));
        }
        result
    }

    async fn dig_send_public_once(
        &self,
        channel_id: i64,
        response: InteractionResponse,
        nonce: &str,
    ) -> Result<InteractionMessageReceipt, DigPublicSendFailure> {
        const BOT_USER_ID: u64 = 8_008;
        let reject_configured = channel_id == CHANNEL as i64 + 1
            && std::mem::replace(
                &mut *self
                    .reject_next_configured_nonce_send
                    .lock()
                    .expect("configured nonce send fault"),
                false,
            );
        if reject_configured {
            return Err(DigPublicSendFailure::rejected(
                "test configured channel rejected the send",
            ));
        }
        if let Some(existing) = self
            .public_history
            .lock()
            .expect("public history")
            .iter()
            .find(|message| {
                message.nonce.as_deref() == Some(nonce)
                    && self
                        .message_channels
                        .lock()
                        .expect("message channels")
                        .get(&message.message_id)
                        .copied()
                        .is_none_or(|message_channel| message_channel == channel_id)
            })
            .cloned()
        {
            return Ok(InteractionMessageReceipt {
                message_id: existing.message_id,
                channel_id: u64::try_from(channel_id)
                    .map_err(|_| DigPublicSendFailure::rejected("negative test channel"))?,
                delivery: crate::registration::InteractionMessageDelivery::ChannelFallback,
            });
        }
        let accept_then_fail = {
            let mut armed = self
                .accept_then_fail_nonce_send
                .lock()
                .expect("nonce send fault");
            let armed_now = *armed;
            *armed = false;
            armed_now
        };
        self.public.lock().expect("public responses").push(response);
        let mut history = self.public_history.lock().expect("public history");
        let message_id = u64::try_from(history.len() + 1)
            .map_err(|_| DigPublicSendFailure::ambiguous("test history overflow"))?;
        history.push(DigPublicHistoryMessage {
            message_id,
            author_id: BOT_USER_ID,
            nonce: Some(nonce.to_owned()),
            interaction_id: None,
            content: String::new(),
            embed_titles: Vec::new(),
            embed_descriptions: Vec::new(),
        });
        self.message_channels
            .lock()
            .expect("message channels")
            .insert(message_id, channel_id);
        if accept_then_fail {
            *self.fail_next_history.lock().expect("history fault") = true;
            return Err(DigPublicSendFailure::ambiguous(
                "test connection lost after Discord accepted the message",
            ));
        }
        Ok(InteractionMessageReceipt {
            message_id,
            channel_id: u64::try_from(channel_id)
                .map_err(|_| DigPublicSendFailure::rejected("negative test channel"))?,
            delivery: crate::registration::InteractionMessageDelivery::ChannelFallback,
        })
    }

    async fn dig_add_reaction(
        &self,
        channel_id: i64,
        message_id: u64,
        emoji: &str,
    ) -> Result<(), String> {
        self.reactions
            .lock()
            .expect("reactions")
            .push((channel_id, message_id, emoji.to_owned()));
        Ok(())
    }

    async fn dig_public_history(
        &self,
        channel_id: i64,
        _after_unix_seconds: i64,
        limit: usize,
    ) -> Result<DigPublicHistory, String> {
        let fail = {
            let mut fail_next = self.fail_next_history.lock().expect("history fault");
            let fail = *fail_next;
            *fail_next = false;
            fail
        };
        if fail {
            return Err("test connection lost before receipt history".to_owned());
        }
        Ok(DigPublicHistory {
            bot_user_id: 8_008,
            messages: self
                .public_history
                .lock()
                .expect("public history")
                .iter()
                .filter(|message| {
                    self.message_channels
                        .lock()
                        .expect("message channels")
                        .get(&message.message_id)
                        .copied()
                        .is_none_or(|message_channel| message_channel == channel_id)
                })
                .rev()
                .take(limit)
                .cloned()
                .collect(),
        })
    }
}

#[derive(Default)]
struct TestResponder {
    defers: StdMutex<Vec<bool>>,
    responses: StdMutex<Vec<InteractionResponse>>,
    followups: StdMutex<Vec<InteractionResponse>>,
    updates: StdMutex<Vec<InteractionResponse>>,
    original_edits: StdMutex<Vec<InteractionResponse>>,
    message_edits: StdMutex<Vec<(InteractionMessageReceipt, InteractionResponse)>>,
    autocompletes: StdMutex<Vec<Vec<CommandOptionChoice>>>,
    modals: StdMutex<Vec<InteractionModal>>,
    lifecycle: Option<Arc<StdMutex<Vec<&'static str>>>>,
}

impl TestResponder {
    fn with_lifecycle(lifecycle: Arc<StdMutex<Vec<&'static str>>>) -> Self {
        Self {
            lifecycle: Some(lifecycle),
            ..Self::default()
        }
    }
}

#[async_trait]
impl InteractionResponder for TestResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.responses.lock().expect("responses").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defers.lock().expect("defers").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.followups.lock().expect("followups").push(response);
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.lock().expect("lifecycle log").push("followup");
        }
        Ok(())
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.followup(response).await?;
        Ok(Some(InteractionMessageReceipt {
            message_id: 99,
            channel_id: CHANNEL,
            delivery: crate::registration::InteractionMessageDelivery::InteractionFollowup,
        }))
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

    async fn edit_message(
        &self,
        receipt: InteractionMessageReceipt,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.message_edits
            .lock()
            .expect("message edits")
            .push((receipt, response));
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

    async fn show_modal(&self, modal: InteractionModal) -> Result<(), InteractionResponseError> {
        self.modals.lock().expect("modals").push(modal);
        Ok(())
    }
}

struct AcceptedThenLostEventResponder {
    inner: Arc<TestResponder>,
    discord: Arc<TestDiscord>,
    interaction_id: u64,
    channel_id: i64,
}

#[async_trait]
impl InteractionResponder for AcceptedThenLostEventResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.inner.respond(response).await
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.inner.defer(ephemeral).await
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        self.inner.followup(response).await
    }

    async fn followup_with_receipt(
        &self,
        response: InteractionResponse,
    ) -> Result<Option<InteractionMessageReceipt>, InteractionResponseError> {
        self.inner.followup(response.clone()).await?;
        self.discord
            .public
            .lock()
            .expect("accepted event public response")
            .push(response.clone());
        let mut history = self
            .discord
            .public_history
            .lock()
            .expect("accepted event history");
        let message_id = u64::try_from(history.len() + 1).expect("event history id");
        history.push(DigPublicHistoryMessage {
            message_id,
            author_id: 8_008,
            nonce: None,
            interaction_id: Some(self.interaction_id),
            content: response.content,
            embed_titles: response
                .embeds
                .iter()
                .map(|embed| embed.title.clone())
                .collect(),
            embed_descriptions: response
                .embeds
                .iter()
                .map(|embed| embed.description.clone())
                .collect(),
        });
        self.discord
            .message_channels
            .lock()
            .expect("accepted event channel")
            .insert(message_id, self.channel_id);
        Err(InteractionResponseError::new(
            "test connection lost after event follow-up acceptance",
        ))
    }

    async fn update(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.inner.update(response).await
    }
}

#[derive(Default)]
struct RejectingPublicFollowupResponder {
    defers: StdMutex<Vec<bool>>,
    attempts: StdMutex<Vec<InteractionResponse>>,
}

#[async_trait]
impl InteractionResponder for RejectingPublicFollowupResponder {
    async fn respond(&self, response: InteractionResponse) -> Result<(), InteractionResponseError> {
        self.attempts.lock().expect("attempts").push(response);
        Ok(())
    }

    async fn defer(&self, ephemeral: bool) -> Result<(), InteractionResponseError> {
        self.defers.lock().expect("defers").push(ephemeral);
        Ok(())
    }

    async fn followup(
        &self,
        response: InteractionResponse,
    ) -> Result<(), InteractionResponseError> {
        let public = !response.ephemeral;
        self.attempts.lock().expect("attempts").push(response);
        if public {
            Err(InteractionResponseError::new("public followup forbidden"))
        } else {
            Ok(())
        }
    }
}

fn config() -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
        "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
        _ => None,
    })
    .expect("provider test config")
}

fn neon_config(chance: &str) -> ApplicationConfig {
    ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
        "NEON_DEGEN_ENABLED" => Some("true".to_owned()),
        "DIG_LLM_ENABLED" => Some("false".to_owned()),
        "NEON_DIG_CHANCE" => Some(chance.to_owned()),
        _ => None,
    })
    .expect("Neon provider test config")
}

fn fixture() -> (NamedTempFile, DigRegistrationProvider, Arc<TestDiscord>) {
    fixture_with_discord(Arc::new(TestDiscord::default()))
}

fn fixture_with_discord(
    discord: Arc<TestDiscord>,
) -> (NamedTempFile, DigRegistrationProvider, Arc<TestDiscord>) {
    fixture_with_discord_and_config(discord, config())
}

fn fixture_with_discord_and_config(
    discord: Arc<TestDiscord>,
    config: ApplicationConfig,
) -> (NamedTempFile, DigRegistrationProvider, Arc<TestDiscord>) {
    let database = NamedTempFile::new().expect("temporary database");
    initialize_or_migrate(database.path()).expect("canonical schema");
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            USER as i64,
            "dig-test-miner",
            Some(GUILD as i64),
        ))
        .expect("registered player");
    Connection::open(database.path())
        .expect("open test database")
        .execute(
            "UPDATE players SET jopacoin_balance=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![500_i64, USER as i64, GUILD as i64],
        )
        .expect("seed balance");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|ancestor| ancestor.join("assets/dig"))
        .find(|candidate| candidate.is_dir())
        .expect("repository Dig asset tree");
    let media = Arc::new(DigMediaRuntime::production(
        &DigRuntimeConfig::with_asset_root(root),
    ));
    let provider =
        DigRegistrationProvider::with_media(database.path(), &config, discord.clone(), None, media);
    (database, provider, discord)
}

fn hook_outcome() -> DigRuntimeOutcome {
    DigRuntimeOutcome {
        success: true,
        error: None,
        depth_before: 100,
        depth_after: 90,
        advance: 1,
        jc_earned: 2,
        vanity_tax: 0,
        low_priority_tax: 0,
        balance_after: 100,
        tunnel_name: "Test Tunnel".to_owned(),
        milestone_bonus: 0,
        streak_bonus: 0,
        bankruptcy_penalty: 0,
        luminosity_after: 100,
        luminosity_drained: 0,
        corruption_description: None,
        mutation_names: Vec::new(),
        tip: "Keep digging!".to_owned(),
        cave_in: false,
        cave_in_detail: None,
        event_id: None,
        artifact_id: None,
        boss_boundary: None,
        first_dig: false,
        paid_dig_cost: 0,
        cooldown_remaining: 0,
        paid_dig_available: false,
        items_used: Vec::new(),
        consumed_item_ids: Vec::new(),
        action_id: Some(11),
        route_choice_required: false,
        pickaxe_tier: 1,
        pet_dig_bonus: 0,
        pet_name: None,
        forced_event_consumed: false,
        relic_trim_notice: false,
        weather: None,
    }
}

#[derive(Default)]
struct RecordingBonusDispatcher {
    bonuses: StdMutex<Vec<cama_app::dig_bonus_events::DigBonus>>,
}

#[async_trait]
impl DigBonusDispatchPort for RecordingBonusDispatcher {
    async fn dispatch_bonus(
        &self,
        _action_id: i64,
        _user_id: i64,
        _guild_id: i64,
        _channel_id: i64,
        bonus: cama_app::dig_bonus_events::DigBonus,
        _responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        self.bonuses.lock().expect("bonus calls").push(bonus);
        Ok(())
    }
}

#[derive(Default)]
struct FailingBonusDispatcher {
    attempts: StdMutex<usize>,
}

#[async_trait]
impl DigBonusDispatchPort for FailingBonusDispatcher {
    async fn dispatch_bonus(
        &self,
        _action_id: i64,
        _user_id: i64,
        _guild_id: i64,
        _channel_id: i64,
        _bonus: cama_app::dig_bonus_events::DigBonus,
        _responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        *self.attempts.lock().expect("bonus attempts") += 1;
        Err("bonus adapter failed after dispatch began".to_owned())
    }
}

#[test]
fn post_result_policy_reuses_authored_flame_copy_and_stable_bonus_roll() {
    assert_eq!(
        super::catastrophic_flame_line(11),
        super::catastrophic_flame_line(11)
    );
    assert!(super::CATASTROPHIC_LINES.contains(&super::catastrophic_flame_line(11)));
    let roll = super::deterministic_dig_bonus_roll(11);
    assert!((0.0..=1.0).contains(&roll));
    assert_eq!(
        cama_app::dig_bonus_events::pick_dig_bonus(roll),
        cama_app::dig_bonus_events::pick_dig_bonus(roll)
    );
}

#[test]
fn pet_activity_source_key_is_action_scoped_and_retry_stable() {
    assert_eq!(
        super::dig_pet_activity_source_key(42),
        super::dig_pet_activity_source_key(42)
    );
    assert_ne!(
        super::dig_pet_activity_source_key(42),
        super::dig_pet_activity_source_key(43)
    );
}

#[test]
fn post_result_policy_maps_only_rare_or_legendary_artifacts_to_neon() {
    assert_eq!(
        super::dig_artifact_neon_info("echo_stone"),
        Some(("Echo Stone".to_owned(), "rare".to_owned()))
    );
    assert_eq!(
        super::dig_artifact_neon_info("hollow_eye"),
        Some(("Hollow Eye".to_owned(), "legendary".to_owned()))
    );
    assert_eq!(super::dig_artifact_neon_info("crystal_compass"), None);
    assert_eq!(
        super::dig_artifact_neon_info("pinnacle:Cloak of the Necropolis:Long Silence"),
        Some(("Cloak of the Necropolis".to_owned(), "legendary".to_owned()))
    );
}

#[test]
fn post_result_reactions_match_python_result_shapes() {
    let mut outcome = hook_outcome();
    outcome.cave_in = true;
    outcome.artifact_id = Some("echo_stone".to_owned());
    assert_eq!(
        super::dig_result_reactions(&outcome, None),
        vec!["⛏️", "💥", "💎"]
    );
    outcome.boss_boundary = Some(101);
    assert_eq!(super::dig_result_reactions(&outcome, None), vec!["💀"]);
    outcome.boss_boundary = None;
    outcome.first_dig = true;
    assert!(super::dig_result_reactions(&outcome, None).is_empty());
}

#[tokio::test]
async fn catastrophic_flame_is_claimed_once_and_never_blocks_dig_delivery() {
    let (_database, provider, discord) = fixture();
    let mut outcome = hook_outcome();
    outcome.cave_in = true;
    outcome.cave_in_detail =
        Some(serde_json::json!({"type": "catastrophic", "block_loss": 10}).to_string());
    provider
        .handler
        .post_catastrophic_flame(11, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
        .await;
    provider
        .handler
        .post_catastrophic_flame(11, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
        .await;
    assert_eq!(discord.public.lock().expect("public responses").len(), 1);
}

#[tokio::test]
async fn catastrophic_flame_transport_error_keeps_terminal_claim() {
    let (_database, provider, discord) = fixture();
    let mut outcome = hook_outcome();
    outcome.cave_in = true;
    outcome.cave_in_detail = Some(serde_json::json!({"type": "catastrophic"}).to_string());
    *discord
        .reject_un_nonnced_public_send
        .lock()
        .expect("send fault") = true;
    provider
        .handler
        .post_catastrophic_flame(12, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
        .await;
    *discord
        .reject_un_nonnced_public_send
        .lock()
        .expect("send fault") = false;
    provider
        .handler
        .post_catastrophic_flame(12, USER as i64, GUILD as i64, &outcome, CHANNEL as i64)
        .await;
    assert!(
        discord.public.lock().expect("public responses").is_empty(),
        "an ambiguous post-send error must not be retried into a duplicate"
    );
}

#[tokio::test]
async fn dig_bonus_roll_and_dispatch_are_stable_across_provider_retry() {
    let (_database, provider, _discord) = fixture();
    let dispatcher = Arc::new(RecordingBonusDispatcher::default());
    provider.set_bonus_dispatcher(dispatcher.clone());
    let action_id = (1_i64..10_000)
        .find(|action_id| {
            cama_app::dig_bonus_events::pick_dig_bonus(super::deterministic_dig_bonus_roll(
                *action_id,
            ))
            .is_some()
        })
        .expect("stable test action should select a bonus");
    let mut outcome = hook_outcome();
    outcome.action_id = Some(action_id);
    let responder: Arc<dyn InteractionResponder> = Arc::new(TestResponder::default());
    provider
        .handler
        .maybe_send_dig_bonus(
            &outcome,
            USER as i64,
            GUILD as i64,
            CHANNEL as i64,
            Arc::clone(&responder),
        )
        .await;
    provider
        .handler
        .maybe_send_dig_bonus(
            &outcome,
            USER as i64,
            GUILD as i64,
            CHANNEL as i64,
            responder,
        )
        .await;
    assert_eq!(dispatcher.bonuses.lock().expect("bonus calls").len(), 1);
}

#[tokio::test]
async fn ambiguous_bonus_dispatch_keeps_claim_terminal_across_retry() {
    let (_database, provider, _discord) = fixture();
    let dispatcher = Arc::new(FailingBonusDispatcher::default());
    provider.set_bonus_dispatcher(dispatcher.clone());
    let action_id = (1_i64..10_000)
        .find(|action_id| {
            cama_app::dig_bonus_events::pick_dig_bonus(super::deterministic_dig_bonus_roll(
                *action_id,
            ))
            .is_some()
        })
        .expect("stable test action should select a bonus");
    let mut outcome = hook_outcome();
    outcome.action_id = Some(action_id);
    let responder: Arc<dyn InteractionResponder> = Arc::new(TestResponder::default());
    provider
        .handler
        .maybe_send_dig_bonus(
            &outcome,
            USER as i64,
            GUILD as i64,
            CHANNEL as i64,
            Arc::clone(&responder),
        )
        .await;
    provider
        .handler
        .maybe_send_dig_bonus(
            &outcome,
            USER as i64,
            GUILD as i64,
            CHANNEL as i64,
            responder,
        )
        .await;
    assert_eq!(*dispatcher.attempts.lock().expect("bonus attempts"), 1);
}

fn go_request() -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id: 1,
        name: "dig".to_owned(),
        user_id: USER,
        user_display_name: "Dig Test Miner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: "go".to_owned(),
            value: InteractionValue::Subcommand(Vec::new()),
        }],
    }
}

fn component_request(custom_id: impl Into<String>, values: Vec<String>) -> InteractionRequest {
    component_request_as(USER, "Dig Test Miner", custom_id, values)
}

fn event_component_id(provider: &DigRegistrationProvider, action_id: i64, choice: &str) -> String {
    format!(
        "dig:event-action:{}:{action_id}:{choice}",
        provider.handler.state.view_nonce
    )
}

fn component_request_as(
    user_id: u64,
    display_name: &str,
    custom_id: impl Into<String>,
    values: Vec<String>,
) -> InteractionRequest {
    InteractionRequest::Component {
        interaction_id: 2,
        custom_id: custom_id.into(),
        user_id,
        user_display_name: display_name.to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        values,
    }
}

fn modal_request(
    custom_id: impl Into<String>,
    fields: impl IntoIterator<Item = (&'static str, &'static str)>,
) -> InteractionRequest {
    InteractionRequest::Modal {
        interaction_id: 3,
        custom_id: custom_id.into(),
        user_id: USER,
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        fields: fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
    }
}

fn command_request(
    interaction_id: u64,
    subcommand: &str,
    options: Vec<InteractionOption>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "dig".to_owned(),
        user_id: USER,
        user_display_name: "Dig Test Miner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(options),
        }],
    }
}

fn grouped_command_request(
    interaction_id: u64,
    group: &str,
    subcommand: &str,
    options: Vec<InteractionOption>,
) -> InteractionRequest {
    InteractionRequest::Command {
        interaction_id,
        name: "dig".to_owned(),
        user_id: USER,
        user_display_name: "Dig Test Miner".to_owned(),
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        member_permissions: None,
        options: vec![InteractionOption {
            name: group.to_owned(),
            value: InteractionValue::SubcommandGroup(vec![InteractionOption {
                name: subcommand.to_owned(),
                value: InteractionValue::Subcommand(options),
            }]),
        }],
    }
}

fn autocomplete_request(
    interaction_id: u64,
    subcommand: &str,
    focused_option: &str,
    focused_value: &str,
) -> InteractionRequest {
    InteractionRequest::Autocomplete {
        interaction_id,
        name: "dig".to_owned(),
        user_id: USER,
        guild_id: Some(GUILD),
        channel_id: Some(CHANNEL),
        focused_option: focused_option.to_owned(),
        focused_value: focused_value.to_owned(),
        options: vec![InteractionOption {
            name: subcommand.to_owned(),
            value: InteractionValue::Subcommand(vec![InteractionOption {
                name: focused_option.to_owned(),
                value: InteractionValue::String(focused_value.to_owned()),
            }]),
        }],
    }
}

#[tokio::test]
async fn accepted_public_delivery_is_reconciled_after_restart_without_duplicate_send() {
    let (database, provider, discord) = fixture();
    let now = super::unix_now();
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            now,
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0xfedc_ba98_7654_3210,
                CHANNEL as i64,
                "Dig Test Miner",
                None,
            ),
        )
        .await
        .expect("commit mechanics and immutable delivery");
    let delivery = execution.delivery.expect("committed delivery snapshot");
    let nonce = super::dig_delivery_nonce(
        &delivery,
        cama_app::dig_runtime::DigRuntimeDeliveryPart::Main,
    );
    assert_eq!(nonce, "cama-d:fedcba9876543210:m");
    assert!(nonce.len() <= 25);

    // Model a process dying immediately after Discord accepts the stable
    // nonce, before mark_delivery_part can commit its SQLite CAS.
    let (main, event) = super::dig_delivery_responses(
        &delivery,
        &provider.handler.state.media,
        &provider.handler.state.view_nonce,
    );
    assert!(event.is_none(), "first Dig has one delivery part");
    discord
        .dig_send_public_once(CHANNEL as i64, main, &nonce)
        .await
        .expect("Discord accepted the message before the crash");
    assert_eq!(discord.public.lock().expect("public responses").len(), 1);
    assert_eq!(
        provider
            .handler
            .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("delivery remains pending before restart")
            .len(),
        1
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    restarted
        .handler
        .deliver_to_channel(&delivery)
        .await
        .expect("restart reconciles the accepted nonce");
    assert_eq!(
        discord.public.lock().expect("single public response").len(),
        1,
        "history reconciliation must not issue a second Discord send"
    );
    assert!(
        restarted
            .handler
            .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("reload reconciled delivery")
            .is_empty(),
        "the recovered receipt completes the durable delivery CAS"
    );
}

#[tokio::test]
async fn accepted_event_delivery_is_reconciled_after_restart_without_duplicate_send() {
    let (database, provider, discord) = fixture();
    let connection = Connection::open(database.path()).expect("open event outbox database");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
              prestige_perks,boss_progress)
             VALUES (?1,?2,30,30,1,100,'[]','{}')",
            params![USER as i64, GUILD as i64],
        )
        .expect("seed event outbox tunnel");
    let prompt_action_id = {
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 30, 30, 0, ?3, ?4)",
                params![
                    GUILD as i64,
                    USER as i64,
                    serde_json::json!({"event":"underground_stream"}).to_string(),
                    super::unix_now() - 1,
                ],
            )
            .expect("seed event outbox prompt");
        connection.last_insert_rowid()
    };
    let service = cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
        database.path(),
        provider.handler.state.dig_config.clone(),
    );
    let outcome = service
        .resolve_action_event_with_delivery(
            cama_app::dig_event_runtime::DigEventActionRequest {
                discord_id: USER as i64,
                guild_id: GUILD as i64,
                dig_action_id: prompt_action_id,
                choice: "safe",
                now: super::unix_now(),
            },
            cama_app::dig_event_runtime::DigEventDeliveryContext::new(
                USER as i64,
                GUILD as i64,
                0x1234_5678,
                CHANNEL as i64,
            ),
        )
        .expect("settle event and attach outbox");
    let action_id = outcome.action_id.expect("resolved event action id");
    let delivery = provider
        .handler
        .event_delivery_for_action(action_id, USER as i64, GUILD as i64)
        .await
        .expect("query event Ready projection")
        .expect("event Ready projection");
    assert_eq!(delivery.context.channel_id, CHANNEL as i64);
    let response = super::event_resolution_response(&delivery.outcome);
    discord
        .dig_send_public_once(CHANNEL as i64, response, &delivery.nonce())
        .await
        .expect("Discord accepted event result before CAS");

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    restarted
        .handler
        .deliver_event_to_channel(&delivery)
        .await
        .expect("restart reconciles accepted event nonce");
    assert_eq!(
        discord.public.lock().expect("single event response").len(),
        1,
        "event history reconciliation must not issue a duplicate send"
    );
    assert!(
        restarted
            .handler
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("query event outbox after restart")
            .is_empty(),
        "event delivery CAS completes after nonce reconciliation"
    );
}

struct EmptyGatewayMembers;

#[async_trait]
impl GuildMemberPageSource for EmptyGatewayMembers {
    async fn fetch_page(
        &self,
        _guild_id: u64,
        _after: Option<u64>,
        _limit: u64,
    ) -> Result<Vec<GatewayMember>, String> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn live_event_followup_acceptance_is_reconciled_by_ready_without_duplicate() {
    let (database, provider, discord) = fixture();
    let connection = Connection::open(database.path()).expect("open live event database");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
              prestige_perks,boss_progress)
             VALUES (?1,?2,30,30,1,100,'[]','{}')",
            params![USER as i64, GUILD as i64],
        )
        .expect("seed live event tunnel");
    let prompt_action_id = {
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 30, 30, 0, ?3, ?4)",
                params![
                    GUILD as i64,
                    USER as i64,
                    serde_json::json!({"event":"underground_stream"}).to_string(),
                    super::unix_now() - 1,
                ],
            )
            .expect("seed live event prompt");
        connection.last_insert_rowid()
    };
    // The follow-up is recorded with the component interaction metadata,
    // then the history read fails. This models a process dying after
    // Discord accepted the message and before the delivery CAS.
    *discord
        .fail_next_history
        .lock()
        .expect("event history fault") = true;
    let inner = Arc::new(TestResponder::default());
    let responder: Arc<dyn InteractionResponder> = Arc::new(AcceptedThenLostEventResponder {
        inner: Arc::clone(&inner),
        discord: Arc::clone(&discord),
        interaction_id: 2,
        channel_id: CHANNEL as i64,
    });
    let result = provider
        .handler
        .handle(
            component_request(
                event_component_id(&provider, prompt_action_id, "safe"),
                Vec::new(),
            ),
            responder,
        )
        .await;
    assert!(
        result.is_err(),
        "the lost history read leaves Ready for recovery"
    );
    assert_eq!(inner.updates.lock().expect("event source update").len(), 1);
    assert_eq!(
        discord.public.lock().expect("accepted event result").len(),
        1
    );
    assert_eq!(
        provider
            .handler
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("query Ready event")
            .len(),
        1
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    let report = restarted
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            vec![GUILD],
            Arc::new(EmptyGatewayMembers),
        ))
        .await;
    assert!(
        report.failures.is_empty(),
        "event READY recovery: {report:?}"
    );
    assert_eq!(report.members_refreshed, 1);
    assert_eq!(
        discord.public.lock().expect("single event result").len(),
        1,
        "interaction-history reconciliation must avoid a nonce repost"
    );
    assert!(
        restarted
            .handler
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("query recovered event")
            .is_empty()
    );
}

#[tokio::test]
async fn ready_recovery_freezes_pending_event_before_sending_once() {
    let (database, provider, discord) = fixture();
    let connection = Connection::open(database.path()).expect("open pending event database");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
              prestige_perks,boss_progress)
             VALUES (?1,?2,30,30,1,100,'[]','{}')",
            params![USER as i64, GUILD as i64],
        )
        .expect("seed pending event tunnel");
    let prompt_action_id = {
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 30, 30, 0, ?3, ?4)",
                params![
                    GUILD as i64,
                    USER as i64,
                    serde_json::json!({"event":"underground_stream"}).to_string(),
                    super::unix_now() - 1,
                ],
            )
            .expect("seed pending event prompt");
        connection.last_insert_rowid()
    };
    let service = cama_app::dig_event_runtime::DigEventRuntimeService::sqlite_with_config(
        database.path(),
        provider.handler.state.dig_config.clone(),
    );
    let outcome = service
        .resolve_action_event_with_delivery(
            cama_app::dig_event_runtime::DigEventActionRequest {
                discord_id: USER as i64,
                guild_id: GUILD as i64,
                dig_action_id: prompt_action_id,
                choice: "safe",
                now: super::unix_now(),
            },
            cama_app::dig_event_runtime::DigEventDeliveryContext::new(
                USER as i64,
                GUILD as i64,
                0xfeed_beef,
                CHANNEL as i64,
            ),
        )
        .expect("settle event outbox");
    let action_id = outcome.action_id.expect("event action id");
    // Simulate a process stopping after actor settlement but before the
    // application-owned quest/finale follow-up froze the projection.
    connection
        .execute(
            "UPDATE dig_actions
                SET detail=json_set(detail, '$.event_delivery.state', 'pending')
              WHERE id=?1 AND action_type='event'",
            params![action_id],
        )
        .expect("rewind event delivery to pending crash state");
    assert_eq!(
        provider
            .handler
            .pending_event_delivery_recoveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("query pending event recovery")
            .len(),
        1
    );

    let report = provider
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            vec![GUILD],
            Arc::new(EmptyGatewayMembers),
        ))
        .await;
    assert!(
        report.failures.is_empty(),
        "pending event recovery: {report:?}"
    );
    assert_eq!(report.members_refreshed, 1);
    assert_eq!(discord.public.lock().expect("event send").len(), 1);
    assert!(
        provider
            .handler
            .pending_event_delivery_recoveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("query recovered pending event")
            .is_empty()
    );
}

#[tokio::test]
async fn configured_public_send_crash_reconciles_without_duplicate() {
    const CONFIGURED_CHANNEL: i64 = CHANNEL as i64 + 1;
    let discord = Arc::new(TestDiscord::with_channels([
        CHANNEL as i64,
        CONFIGURED_CHANNEL,
    ]));
    discord.arm_accept_then_fail_nonce_send();
    discord.reject_un_nonnced_public_send();
    let configured = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
        "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
        "DIG_CHANNEL_ID" => Some(CONFIGURED_CHANNEL.to_string()),
        _ => None,
    })
    .expect("configured Dig provider test config");
    let (database, provider, discord) =
        fixture_with_discord_and_config(discord, configured.clone());
    let responder = Arc::new(TestResponder::default());

    // The configured-channel send accepts the nonce-addressed message,
    // then the process loses its connection before the durable CAS.  The
    // provider must not fall back to an un-nonced public send in this
    // ambiguous state.
    let initial = provider.handler.handle(go_request(), responder).await;
    assert!(initial.is_err());
    assert_eq!(
        discord.public.lock().expect("public responses").len(),
        1,
        "Discord accepted exactly one configured-channel message"
    );
    assert_eq!(
        discord.public_history.lock().expect("public history").len(),
        1
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &configured,
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    let report = restarted
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            vec![GUILD],
            Arc::new(EmptyGatewayMembers),
        ))
        .await;
    assert!(report.failures.is_empty(), "recovery report: {report:?}");
    assert_eq!(report.members_refreshed, 1);
    assert_eq!(
        discord.public.lock().expect("single public response").len(),
        1,
        "READY reconciliation must not issue a duplicate configured send"
    );
    assert!(
        restarted
            .handler
            .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("pending recovery query")
            .is_empty(),
        "receipt recovery completes the durable delivery CAS"
    );
}

#[tokio::test]
async fn configured_fallback_crash_reconciles_without_duplicate_after_rebind() {
    const CONFIGURED_CHANNEL: i64 = CHANNEL as i64 + 1;
    let discord = Arc::new(TestDiscord::with_channels([
        CHANNEL as i64,
        CONFIGURED_CHANNEL,
    ]));
    discord.reject_next_configured_nonce_send();
    discord.arm_accept_then_fail_nonce_send();
    discord.reject_un_nonnced_public_send();
    let configured = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
        "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
        "DIG_CHANNEL_ID" => Some(CONFIGURED_CHANNEL.to_string()),
        _ => None,
    })
    .expect("configured Dig provider test config");
    let (database, provider, discord) =
        fixture_with_discord_and_config(discord, configured.clone());
    let responder = Arc::new(TestResponder::default());

    // Configured-channel rejection falls back to the interaction channel;
    // the fallback is accepted, then the process loses the CAS window.
    assert!(
        provider
            .handler
            .handle(go_request(), responder)
            .await
            .is_err()
    );
    assert_eq!(
        discord
            .public
            .lock()
            .expect("fallback public response")
            .len(),
        1
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &configured,
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    let report = restarted
        .gateway_observer()
        .ready_recovery(ReadyRecoveryContext::new(
            vec![GUILD],
            Arc::new(EmptyGatewayMembers),
        ))
        .await;
    assert!(report.failures.is_empty(), "recovery report: {report:?}");
    assert_eq!(
        discord.public.lock().expect("public responses").len(),
        1,
        "persisted fallback-channel rebind must prevent configured-channel repost"
    );
}

#[tokio::test]
async fn pending_configured_delivery_rejection_falls_back_to_interaction_channel() {
    const CONFIGURED_CHANNEL: i64 = CHANNEL as i64 + 1;
    let discord = Arc::new(TestDiscord::with_channels([
        CHANNEL as i64,
        CONFIGURED_CHANNEL,
    ]));
    let configured = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("dig-provider-test-token".to_owned()),
        "NEON_DEGEN_ENABLED" => Some("false".to_owned()),
        "DIG_CHANNEL_ID" => Some(CONFIGURED_CHANNEL.to_string()),
        _ => None,
    })
    .expect("configured Dig provider test config");
    let (_database, provider, discord) = fixture_with_discord_and_config(discord, configured);
    provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0xdead_beef,
                CONFIGURED_CHANNEL,
                "Dig Test Miner",
                None,
            ),
        )
        .await
        .expect("commit pending configured-channel delivery");
    discord.reject_next_configured_nonce_send();
    let responder = Arc::new(TestResponder::default());

    provider
        .handler
        .handle(go_request(), responder.clone())
        .await
        .expect("safe recovery rejection should fall back to the interaction channel");

    assert_eq!(discord.public.lock().expect("recovered delivery").len(), 1);
    assert!(
        discord
            .message_channels
            .lock()
            .expect("delivery channels")
            .values()
            .all(|channel| *channel == CHANNEL as i64)
    );
    assert!(
        provider
            .handler
            .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("pending delivery query")
            .is_empty()
    );
    assert_eq!(responder.followups.lock().expect("current go").len(), 1);
}

#[tokio::test]
async fn accepted_interaction_followup_is_reconciled_by_interaction_and_immutable_body() {
    let (database, provider, discord) = fixture();
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0x1234_5678,
                CHANNEL as i64,
                "Dig Test Miner",
                None,
            ),
        )
        .await
        .expect("commit immutable followup delivery");
    let delivery = execution.delivery.expect("delivery snapshot");
    let (main, event) = super::dig_delivery_responses(
        &delivery,
        &provider.handler.state.media,
        &provider.handler.state.view_nonce,
    );
    assert!(event.is_none());

    // Interaction followups do not accept a Discord nonce. HTTP history
    // still exposes the originating interaction ID, so the immutable
    // title/description body supplies a stable, per-part discriminator.
    discord
        .public
        .lock()
        .expect("public responses")
        .push(main.clone());
    discord
        .public_history
        .lock()
        .expect("public history")
        .push(DigPublicHistoryMessage {
            message_id: 91,
            author_id: 8_008,
            nonce: None,
            interaction_id: Some(delivery.context.interaction_id),
            content: main.content.clone(),
            embed_titles: main
                .embeds
                .iter()
                .map(|embed| embed.title.clone())
                .collect(),
            embed_descriptions: main
                .embeds
                .iter()
                .map(|embed| embed.description.clone())
                .collect(),
        });

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    restarted
        .handler
        .deliver_to_channel(&delivery)
        .await
        .expect("restart reconciles accepted interaction followup");
    assert_eq!(discord.public.lock().expect("public responses").len(), 1);
    assert!(
        restarted
            .handler
            .pending_deliveries(cama_app::dig_runtime::DigRuntimePendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("pending queue")
            .is_empty()
    );
}

#[tokio::test]
async fn provider_go_route_event_and_info_use_typed_app_service() {
    let (database, provider, discord) = fixture();
    let go_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(go_request(), go_responder.clone())
        .await
        .expect("/dig go should be admitted");

    assert_eq!(
        go_responder.defers.lock().expect("defers").as_slice(),
        &[false]
    );
    {
        let followups = go_responder.followups.lock().expect("followups");
        assert_eq!(followups.len(), 1);
        assert_eq!(followups[0].embeds.len(), 1);
        assert_eq!(
            followups[0].embeds[0].title.as_deref(),
            Some("Welcome to the Mines!")
        );
        assert_eq!(
            followups[0].embeds[0].description.as_deref(),
            Some(
                "You've started digging your very own tunnel!\n\nUse `/dig` to advance deeper, `/dig shop` to buy items, and `/dig guide` for a full tutorial.\n\nGood luck, miner! **DIG DUG!**"
            )
        );
        assert!(followups[0].embeds[0].fields.is_empty());
    }
    assert!(discord.public.lock().expect("public responses").is_empty());

    let service = DigRuntimeService::sqlite(database.path());
    let first = service
        .snapshot(USER as i64, GUILD as i64)
        .expect("typed snapshot after go");
    assert_eq!(
        first.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
        Some(1)
    );
    assert_eq!(
        first.gear.len(),
        1,
        "starter gear is committed with the first dig"
    );

    let trap_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(3, "trap", Vec::new()),
            trap_responder.clone(),
        )
        .await
        .expect("trap should use typed app mutation");
    assert!(
        trap_responder
            .followups
            .lock()
            .expect("trap followups")
            .last()
            .is_some_and(|response| response.content.contains("Trap set!"))
    );
    assert!(!trap_responder.followups.lock().unwrap()[0].ephemeral);
    assert_eq!(trap_responder.defers.lock().unwrap().as_slice(), &[false]);
    let insure_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(4, "insure", Vec::new()),
            insure_responder.clone(),
        )
        .await
        .expect("insurance should use typed app mutation");
    assert!(
        insure_responder
            .followups
            .lock()
            .expect("insurance followups")
            .last()
            .is_some_and(|response| response.content.contains("Insurance purchased"))
    );
    assert!(!insure_responder.followups.lock().unwrap()[0].ephemeral);
    assert_eq!(insure_responder.defers.lock().unwrap().as_slice(), &[false]);

    let weather_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(5, "weather", Vec::new()),
            weather_responder.clone(),
        )
        .await
        .expect("weather should use typed app read model");
    assert!(
        weather_responder
            .responses
            .lock()
            .expect("weather responses")
            .last()
            .is_some_and(|response| !response.embeds.is_empty())
    );

    let action_count: i64 = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT COUNT(*) FROM dig_actions
             WHERE actor_id=?1 AND guild_id=?2 AND action_type='dig'",
            params![USER as i64, GUILD as i64],
            |row| row.get(0),
        )
        .expect("dig audit count");
    assert_eq!(action_count, 1);

    // Seed one canonical pending route exactly as the migrated route
    // repository serializes it, then drive the real component adapter.
    let pending_route = r#"{"layer":"Stone","start_depth":25,"end_depth":50,"offered":["shored_passage","old_supports","fossil_seam"],"selected":null}"#;
    Connection::open(database.path())
        .expect("open route fixture")
        .execute(
            "UPDATE tunnels SET route_state=?1 WHERE discord_id=?2 AND guild_id=?3",
            params![pending_route, USER as i64, GUILD as i64],
        )
        .expect("seed pending route");
    let route_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request("dig:route:old_supports", vec!["old_supports".to_owned()]),
            route_responder.clone(),
        )
        .await
        .expect("legacy route component should be rejected safely");
    assert!(
        route_responder
            .responses
            .lock()
            .expect("route responses")
            .last()
            .is_some_and(|response| response.content.contains("expired after a restart"))
    );
    let routed = service
        .snapshot(USER as i64, GUILD as i64)
        .expect("typed snapshot after route");
    assert!(
        routed
            .tunnel
            .as_ref()
            .and_then(|tunnel| tunnel.route_state.as_deref())
            .and_then(|state| cama_app::dig_routes::parse_route_state(Some(state)))
            .and_then(|state| state.selected)
            .is_none()
    );

    // Event choices are also settled by the app service; the provider
    // only validates ownership and presents the typed action result.
    let event_action_id = {
        let connection = Connection::open(database.path()).expect("open event fixture");
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id, actor_id, target_id, action_type, depth_before,
                     depth_after, jc_delta, detail, created_at
                 ) VALUES (?1, ?2, NULL, 'dig', 5, 5, 0, ?3, ?4)",
                params![
                    GUILD as i64,
                    USER as i64,
                    serde_json::json!({"event":"underground_stream"}).to_string(),
                    super::unix_now() - 1
                ],
            )
            .expect("seed durable event action");
        connection.last_insert_rowid()
    };
    let event_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                event_component_id(&provider, event_action_id, "safe"),
                Vec::new(),
            ),
            event_responder.clone(),
        )
        .await
        .expect("event component should be admitted");
    {
        let updates = event_responder.updates.lock().expect("event updates");
        assert_eq!(updates.len(), 1);
        assert!(
            updates[0].embeds.is_empty(),
            "component-only update preserves the authored source embed"
        );
        assert_eq!(updates[0].components.len(), 1);
        assert!(
            updates[0].components[0]
                .buttons
                .iter()
                .all(|button| button.disabled),
            "the source event is locked before its result is published"
        );
    }
    {
        let followups = event_responder.followups.lock().expect("event followup");
        assert_eq!(followups.len(), 1);
        assert_eq!(followups[0].embeds.len(), 1);
        assert_eq!(
            followups[0].embeds[0].description.as_deref(),
            Some("You cross safely and find coins on the far bank.")
        );
    }
    assert!(
        provider
            .handler
            .pending_event_deliveries(DigEventPendingDeliveryQuery {
                guild_id: Some(GUILD as i64),
                discord_id: Some(USER as i64),
                limit: 10,
            })
            .await
            .expect("query resolved event delivery")
            .is_empty(),
        "the resolved action's event outbox is marked, not the prompt action"
    );

    let info_responder = Arc::new(TestResponder::default());
    Connection::open(database.path())
        .expect("open hard-hat info fixture")
        .execute(
            "UPDATE tunnels SET hard_hat_charges=2 WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
        )
        .expect("seed hard-hat charges");
    provider
        .handler
        .handle(
            command_request(4, "info", Vec::new()),
            info_responder.clone(),
        )
        .await
        .expect("info should read typed app projection");
    {
        let info = info_responder.responses.lock().expect("info responses");
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].embeds.len(), 1);
        assert!(
            info[0].embeds[0]
                .fields
                .iter()
                .any(|field| field.name == "Depth")
        );
        assert!(
            info[0].embeds[0]
                .fields
                .iter()
                .any(|field| { field.name == "Protection" && field.value == "Hard Hat (2)" })
        );
    }

    let action_count: i64 = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT COUNT(*) FROM dig_actions WHERE actor_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| row.get(0),
        )
        .expect("action count after components");
    assert_eq!(
        action_count, 4,
        "go, terminal flavor receipt, durable event source, and event choice are audited"
    );
}

#[tokio::test]
async fn event_components_lock_source_publish_once_and_reject_forged_choices() {
    const OTHER: u64 = 77_099;
    let (database, provider, discord) = fixture();
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            OTHER as i64,
            "event-copycat",
            Some(GUILD as i64),
        ))
        .expect("register copied-component actor");
    let connection = Connection::open(database.path()).expect("open event database");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,total_digs,luminosity,
              prestige_perks,boss_progress)
             VALUES (?1,?2,30,30,1,100,'[]','{}')",
            params![USER as i64, GUILD as i64],
        )
        .expect("seed event tunnel");
    let seed_action = |event_id: &str| {
        connection
            .execute(
                "INSERT INTO dig_actions (
                     guild_id,actor_id,target_id,action_type,depth_before,
                     depth_after,jc_delta,detail,created_at
                 ) VALUES (?1,?2,NULL,'dig',30,30,0,?3,?4)",
                params![
                    GUILD as i64,
                    USER as i64,
                    serde_json::json!({"event":event_id}).to_string(),
                    super::unix_now() - 1,
                ],
            )
            .expect("seed event action");
        connection.last_insert_rowid()
    };
    let choice_action = seed_action("underground_stream");

    let copied = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request_as(
                OTHER,
                "Event Copycat",
                event_component_id(&provider, choice_action, "safe"),
                Vec::new(),
            ),
            copied.clone(),
        )
        .await
        .expect("copied event is rejected cleanly");
    assert!(copied.updates.lock().expect("copied updates").is_empty());
    assert_eq!(
        copied.responses.lock().expect("copied response")[0].content,
        "This isn't your event."
    );

    let forged = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                event_component_id(&provider, choice_action, "boon_99"),
                Vec::new(),
            ),
            forged.clone(),
        )
        .await
        .expect("forged choice is rejected cleanly");
    assert!(forged.updates.lock().expect("forged updates").is_empty());
    assert_eq!(
        forged.responses.lock().expect("forged response")[0].content,
        "That event choice is no longer available."
    );

    let boon_action = seed_action("enchanting_table");
    drop(connection);
    let boon_custom_id = event_component_id(&provider, boon_action, "boon_1");
    let first = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(boon_custom_id.clone(), Vec::new()),
            first.clone(),
        )
        .await
        .expect("boon choice resolves");
    {
        let updates = first.updates.lock().expect("boon source update");
        assert_eq!(updates.len(), 1);
        assert!(updates[0].embeds.is_empty());
        assert_eq!(updates[0].components[0].buttons.len(), 3);
        assert!(
            updates[0].components[0]
                .buttons
                .iter()
                .all(|button| button.disabled)
        );
    }
    {
        let followups = first.followups.lock().expect("boon followup result");
        assert_eq!(followups.len(), 1);
        assert_eq!(
            followups[0].embeds[0].title.as_deref(),
            Some("Enchanting Table")
        );
        assert!(
            followups[0].embeds[0]
                .fields
                .iter()
                .any(|field| field.name.starts_with("Buff:"))
        );
    }

    let duplicate = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(boon_custom_id.clone(), Vec::new()),
            duplicate.clone(),
        )
        .await
        .expect("duplicate delivery replays receipt safely");
    assert_eq!(duplicate.updates.lock().expect("duplicate lock").len(), 1);
    {
        let followups = duplicate.followups.lock().expect("duplicate followup");
        assert_eq!(followups.len(), 1);
        assert!(followups[0].ephemeral);
        assert_eq!(followups[0].content, "You've already resolved this event.");
    }
    assert_eq!(
        discord.public.lock().expect("single public result").len(),
        0,
        "event results stay on the interaction follow-up path"
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        discord.clone(),
        None,
        provider.handler.state.media.clone(),
    );
    assert_ne!(
        restarted.handler.state.view_nonce,
        provider.handler.state.view_nonce
    );
    let stale = Arc::new(TestResponder::default());
    restarted
        .handler
        .handle(component_request(boon_custom_id, Vec::new()), stale.clone())
        .await
        .expect("pre-restart event control expires cleanly");
    assert!(stale.updates.lock().expect("stale updates").is_empty());
    let responses = stale.responses.lock().expect("stale response");
    assert_eq!(responses.len(), 1);
    assert!(responses[0].ephemeral);
    assert_eq!(responses[0].content, "This Dig event expired.");
}

#[tokio::test]
async fn boss_encounter_modal_and_durable_duel_execute_live_policy() {
    let (database, provider, _discord) = fixture();
    let fixture_connection = Connection::open(database.path()).expect("boss fixture DB");
    fixture_connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
              boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
             VALUES (?1,?2,24,24,0,?3,0,0,50,5,'Provider Boss')",
            params![
                USER as i64,
                GUILD as i64,
                r#"{"25":{"boss_id":"grothak","status":"active"}}"#,
            ],
        )
        .expect("boss tunnel");
    fixture_connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'lantern',0,100)",
            params![USER as i64, GUILD as i64],
        )
        .expect("scout lantern");

    let encounter = provider
        .handler
        .render_boss_encounter(USER as i64, GUILD as i64, super::unix_now())
        .await
        .expect("encounter response");
    assert_eq!(
        encounter.embeds[0].title.as_deref(),
        Some("Boss Encountered: Grothak the Unbreakable!")
    );
    assert_eq!(encounter.components.len(), 1);
    assert_eq!(
        encounter.components[0]
            .buttons
            .iter()
            .map(|button| button.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Fight", "Retreat", "Scout", "Cheer"]
    );
    assert!(
        encounter.components[0]
            .buttons
            .iter()
            .find(|button| button.label == "Scout")
            .is_some_and(|button| !button.disabled)
    );
    assert!(
        !encounter.attachments.is_empty(),
        "authored or native boss art"
    );

    let scout = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(format!("dig:boss:scout:{USER}:{GUILD}"), Vec::new()),
            scout.clone(),
        )
        .await
        .expect("scout executes live policy");
    {
        let scout_followups = scout.followups.lock().expect("scout followups");
        assert_eq!(
            scout_followups[0].embeds[0].title.as_deref(),
            Some("Boss Scouted")
        );
        assert!(
            scout_followups[0].embeds[0]
                .description
                .as_deref()
                .is_some_and(|description| description.contains("Cautious")
                    && description.contains("free")
                    && description.contains("payout"))
        );
    }
    assert_eq!(
        fixture_connection
            .query_row(
                "SELECT COUNT(*) FROM dig_inventory
                  WHERE discord_id=?1 AND guild_id=?2 AND item_type='lantern'",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("lantern count"),
        0
    );

    const CHEERER: u64 = USER + 1;
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            CHEERER as i64,
            "provider-cheerer",
            Some(GUILD as i64),
        ))
        .expect("cheerer player");
    let cheer = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request_as(
                CHEERER,
                "Cheer Miner",
                format!("dig:boss:cheer:{USER}:{GUILD}"),
                Vec::new(),
            ),
            cheer.clone(),
        )
        .await
        .expect("cheer executes live policy");
    assert!(
        cheer.followups.lock().expect("cheer followups")[0]
            .content
            .contains("+5% (1/3 cheers)")
    );

    let fight = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(format!("dig:boss:fight:{USER}:{GUILD}"), Vec::new()),
            fight.clone(),
        )
        .await
        .expect("fight opens modal");
    let modal = fight.modals.lock().expect("fight modals")[0].clone();
    assert_eq!(modal.title, "Boss Fight Wager");
    assert_eq!(
        modal
            .inputs
            .iter()
            .map(|input| (input.custom_id.as_str(), input.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("risk_tier", "Risk Tier (cautious / bold / reckless)"),
            ("wager", "Wager Amount (max 1,000 JC)"),
        ]
    );

    let submitted = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            modal_request(modal.custom_id, [("risk_tier", "bold"), ("wager", "10")]),
            submitted.clone(),
        )
        .await
        .expect("modal executes boss policy");
    let first = submitted.followups.lock().expect("boss followups")[0].clone();
    assert_ne!(
        first.content,
        format!(
            "Boss wager **10** {} received. Use `/dig go` to continue.",
            cama_domain::formatting::JOPACOIN_EMOTE
        ),
        "the retired echo-only modal path must never return"
    );

    let duel_button = first
        .components
        .iter()
        .flat_map(|row| &row.buttons)
        .find(|button| button.custom_id.starts_with("dig:boss:duel:"))
        .map(|button| button.custom_id.clone());
    if let Some(duel_button) = duel_button {
        // Recreate the provider to prove the component is backed by the
        // durable SQLite duel rather than process-local View state.
        let restarted = DigRegistrationProvider::with_media(
            database.path(),
            &config(),
            Arc::new(TestDiscord::default()),
            None,
            Arc::clone(&provider.handler.state.media),
        );
        let resumed = Arc::new(TestResponder::default());
        restarted
            .handler
            .handle(component_request(duel_button, Vec::new()), resumed.clone())
            .await
            .expect("restart resumes durable duel");
        assert_eq!(resumed.followups.lock().expect("resume followups").len(), 1);
    }
    let connection = Connection::open(database.path()).expect("verify boss DB");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM dig_active_duels", [], |row| row
                .get::<_, i64>(0),)
            .expect("active duel count"),
        0,
        "the live result either resolved directly or was resumed exactly once"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM dig_actions WHERE action_type='boss_fight'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("boss audit count"),
        1
    );
}

#[tokio::test]
async fn boss_retreat_component_uses_atomic_application_settlement() {
    let (database, provider, _discord) = fixture();
    Connection::open(database.path())
        .expect("retreat fixture DB")
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
              boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
             VALUES (?1,?2,24,24,0,?3,0,0,100,5,'Retreat Boss')",
            params![
                USER as i64,
                GUILD as i64,
                r#"{"25":{"boss_id":"grothak","status":"active"}}"#,
            ],
        )
        .expect("retreat tunnel");
    let response = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(format!("dig:boss:retreat:{USER}:{GUILD}"), Vec::new()),
            response.clone(),
        )
        .await
        .expect("retreat component");
    assert!(
        response.followups.lock().expect("retreat followups")[0]
            .content
            .starts_with("You retreated safely, losing")
    );
    let connection = Connection::open(database.path()).expect("verify retreat DB");
    let (depth, audits): (i64, i64) = connection
        .query_row(
            "SELECT t.depth,
                    (SELECT COUNT(*) FROM dig_actions a
                      WHERE a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                        AND a.action_type='boss_retreat')
               FROM tunnels t WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("retreat state");
    assert!((21..=22).contains(&depth));
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn abandon_preview_owner_restart_cancel_and_atomic_settlement_are_exact() {
    let (database, provider, _discord) = fixture();
    let connection = Connection::open(database.path()).expect("abandon fixture DB");
    connection
        .execute(
            "INSERT INTO tunnels (
                discord_id,guild_id,depth,max_depth,total_digs,total_jc_earned,
                pickaxe_tier,prestige_level,boss_progress,streak_days,
                pinnacle_boss_id,pinnacle_phase,route_state
             ) VALUES (
                ?1,?2,100,350,99,777,4,2,'{\"25\":\"defeated\"}',8,
                'forgotten_king',2,'{\"selected\":\"old_supports\"}'
             )",
            params![USER as i64, GUILD as i64],
        )
        .expect("seed abandon tunnel");

    let preview_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(58, "abandon", Vec::new()),
            preview_responder.clone(),
        )
        .await
        .expect("abandon preview");
    let preview = preview_responder.responses.lock().unwrap()[0].clone();
    assert!(!preview.ephemeral);
    assert_eq!(preview.embeds[0].title.as_deref(), Some("Abandon Tunnel?"));
    assert!(
        preview.embeds[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains("Refund: **7**"))
    );
    assert_eq!(preview.components[0].buttons.len(), 2);
    let confirm_id = preview.components[0].buttons[0].custom_id.clone();

    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            USER as i64 + 1,
            "abandon-copy",
            Some(GUILD as i64),
        ))
        .expect("register copied-view actor");
    let copied = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request_as(USER + 1, "Copy", &confirm_id, Vec::new()),
            copied.clone(),
        )
        .await
        .expect("copied abandon rejected");
    assert!(
        copied.responses.lock().unwrap()[0]
            .content
            .contains("isn't your tunnel")
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        Arc::new(TestDiscord::default()),
        None,
        Arc::clone(&provider.handler.state.media),
    );
    let expired = Arc::new(TestResponder::default());
    restarted
        .handler
        .handle(component_request(&confirm_id, Vec::new()), expired.clone())
        .await
        .expect("restart expires abandon view");
    assert!(
        expired.responses.lock().unwrap()[0]
            .content
            .contains("expired")
    );

    let legacy = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request("dig:abandon:confirm", Vec::new()),
            legacy.clone(),
        )
        .await
        .expect("unowned legacy abandon expires");
    assert!(
        legacy.responses.lock().unwrap()[0]
            .content
            .contains("expired")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100
    );

    let committed = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(&confirm_id, Vec::new()),
            committed.clone(),
        )
        .await
        .expect("abandon commits");
    let update = committed.updates.lock().unwrap()[0].clone();
    assert_eq!(
        update.content,
        format!("Tunnel abandoned. You received **7** {JOPACOIN_EMOTE}.")
    );
    assert!(update.components.is_empty());
    let state: (
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
        i64,
        i64,
    ) = connection
        .query_row(
            "SELECT depth,max_depth,total_digs,total_jc_earned,prestige_level,
                        pinnacle_boss_id,route_state,
                        (SELECT jopacoin_balance FROM players
                          WHERE discord_id=t.discord_id AND guild_id=t.guild_id),
                        (SELECT COUNT(*) FROM dig_actions a
                          WHERE a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                            AND a.action_type='abandon')
                   FROM tunnels t WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        state,
        (0, 350, 99, 777, 2, None, None, 507, 1),
        "only Python's partial reset fields change"
    );

    let duplicate = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(&confirm_id, Vec::new()),
            duplicate.clone(),
        )
        .await
        .expect("duplicate abandon is harmless");
    assert!(
        duplicate.responses.lock().unwrap()[0]
            .content
            .contains("already answered")
    );

    connection
        .execute(
            "UPDATE tunnels SET depth=100 WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
        )
        .unwrap();
    let cancel_preview = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(59, "abandon", Vec::new()),
            cancel_preview.clone(),
        )
        .await
        .unwrap();
    let cancel_id = cancel_preview.responses.lock().unwrap()[0].components[0].buttons[1]
        .custom_id
        .clone();
    let cancelled = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(component_request(&cancel_id, Vec::new()), cancelled.clone())
        .await
        .unwrap();
    assert_eq!(
        cancelled.updates.lock().unwrap()[0].content,
        "Abandon cancelled."
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        100
    );
}

#[test]
fn abandon_view_owner_timeout_and_claim_boundary_are_exact() {
    let (_database, provider, _discord) = fixture();
    let token = provider
        .handler
        .create_abandon_view(USER as i64, GUILD as i64, 100)
        .unwrap();
    assert_eq!(
        provider
            .handler
            .claim_abandon_view(&token, USER as i64 + 1, GUILD as i64, 159)
            .unwrap(),
        DigAbandonViewAdmission::WrongOwner
    );
    assert_eq!(
        provider
            .handler
            .claim_abandon_view(&token, USER as i64, GUILD as i64, 159)
            .unwrap(),
        DigAbandonViewAdmission::Admitted
    );
    assert_eq!(
        provider
            .handler
            .claim_abandon_view(&token, USER as i64, GUILD as i64, 159)
            .unwrap(),
        DigAbandonViewAdmission::AlreadyResolved
    );
    let expired = provider
        .handler
        .create_abandon_view(USER as i64, GUILD as i64, 200)
        .unwrap();
    assert_eq!(
        provider
            .handler
            .claim_abandon_view(&expired, USER as i64, GUILD as i64, 260)
            .unwrap(),
        DigAbandonViewAdmission::Expired
    );
}

#[tokio::test]
async fn guide_has_four_exact_owner_bound_pages_and_expires_at_180_seconds() {
    let (_database, provider, _discord) = fixture();
    let token = provider
        .handler
        .create_guide_view(USER as i64, GUILD as i64, 100)
        .expect("create guide view");
    assert_eq!(
        provider
            .handler
            .navigate_guide_view(
                &token,
                USER as i64 + 1,
                GUILD as i64,
                super::DigGuideDirection::Next,
                279,
            )
            .expect("wrong owner admission"),
        super::DigGuideViewAdmission::WrongOwner
    );
    for expected_page in 1..super::DIG_GUIDE_PAGES.len() {
        assert_eq!(
            provider
                .handler
                .navigate_guide_view(
                    &token,
                    USER as i64,
                    GUILD as i64,
                    super::DigGuideDirection::Next,
                    279,
                )
                .expect("owner navigation"),
            super::DigGuideViewAdmission::Admitted(expected_page)
        );
    }
    assert_eq!(
        provider
            .handler
            .navigate_guide_view(
                &token,
                USER as i64,
                GUILD as i64,
                super::DigGuideDirection::Next,
                279,
            )
            .expect("last-page clamp"),
        super::DigGuideViewAdmission::Admitted(3)
    );
    assert_eq!(
        provider
            .handler
            .navigate_guide_view(
                &token,
                USER as i64,
                GUILD as i64,
                super::DigGuideDirection::Previous,
                280,
            )
            .expect("inclusive timeout"),
        super::DigGuideViewAdmission::Expired
    );

    let pages = (0..super::DIG_GUIDE_PAGES.len())
        .map(|page| super::guide_response(page, "owner-token"))
        .collect::<Vec<_>>();
    assert_eq!(
        pages
            .iter()
            .map(|response| response.embeds[0].title.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "Dig Guide — Basics",
            "Dig Guide — Items & Pickaxes",
            "Dig Guide — Bosses",
            "Dig Guide — Prestige",
        ]
    );
    assert_eq!(
        pages
            .iter()
            .map(|response| response.embeds[0].color)
            .collect::<Vec<_>>(),
        [
            Some(0x8B_45_13),
            Some(0x80_80_80),
            Some(0x00_CE_D1),
            Some(0xFF_45_00),
        ]
    );
    assert!(pages[0].components[0].buttons[0].disabled);
    assert!(pages[3].components[0].buttons[1].disabled);
    assert!(pages.iter().all(|response| {
        response.components[0]
            .buttons
            .iter()
            .all(|button| button.custom_id.contains("owner-token"))
    }));
    let expired = super::expired_guide_response();
    assert_eq!(expired.content, "*The moment passed.*");
    assert!(
        expired.components[0]
            .buttons
            .iter()
            .all(|button| button.disabled)
    );

    let (_restarted_database, restarted, _discord) = fixture();
    assert_eq!(
        restarted
            .handler
            .navigate_guide_view(
                &token,
                USER as i64,
                GUILD as i64,
                super::DigGuideDirection::Next,
                101,
            )
            .expect("restart admission"),
        super::DigGuideViewAdmission::Expired
    );
}

#[test]
fn flex_projection_renders_empty_roast_and_full_profile_exactly() {
    let empty = cama_app::dig_runtime::DigRuntimeFlexData {
        tunnel_name: "Unknown".to_owned(),
        depth: 0,
        total_digs: 1,
        total_jc_earned: 0,
        prestige_level: 0,
        prestige_emoji: String::new(),
        titles: Vec::new(),
        streak: 0,
        layer: "Dirt".to_owned(),
    };
    let response = super::flex_response(
        &empty,
        "Fresh Miner",
        Some("https://cdn.example/avatar.png"),
        7,
    );
    let embed = &response.embeds[0];
    assert_eq!(embed.title.as_deref(), Some("Fresh Miner's Mining Profile"));
    assert_eq!(embed.color, Some(0xFF_D7_00));
    assert_eq!(
        embed.description.as_deref(),
        Some("*Your pickaxe is still in the shrinkwrap.*")
    );
    assert!(embed.fields.is_empty());
    assert_eq!(
        embed.thumbnail_url.as_deref(),
        Some("https://cdn.example/avatar.png")
    );

    let veteran = cama_app::dig_runtime::DigRuntimeFlexData {
        tunnel_name: "Veteran Shaft".to_owned(),
        depth: 205,
        total_digs: 123,
        total_jc_earned: 456,
        prestige_level: 8,
        prestige_emoji: "⭐⭐⭐⭐⭐".to_owned(),
        titles: vec!["Boss Slayer".to_owned()],
        streak: 9,
        layer: "Frozen Core".to_owned(),
    };
    let response = super::flex_response(&veteran, "Veteran", None, 0);
    let embed = &response.embeds[0];
    assert_eq!(
        embed.description.as_deref(),
        Some("*\"Boss Slayer\"*  ⭐⭐⭐⭐⭐")
    );
    assert_eq!(embed.fields.len(), 1);
    assert_eq!(embed.fields[0].name, "Stats");
    assert_eq!(
        embed.fields[0].value,
        "Tunnel: **Veteran Shaft**\nDepth: **205** (Frozen Core)\nTotal digs: **123**\nTotal JC earned: **456**\nStreak: **9** days\nPrestige: **8**"
    );
}

#[test]
fn weather_effect_copy_matches_every_python_effect_phrase() {
    let effects = cama_app::dig_runtime::DigWeatherEffects {
        cave_in_bonus: -0.1,
        cave_in_loss_bonus: 0,
        cave_in_loss_cap: None,
        advance_bonus: 1,
        event_chance_multiplier: 0.5,
        luminosity_drain_multiplier: 0.5,
        jc_multiplier: -0.25,
        jc_bonus: 2,
        artifact_multiplier: 3.0,
    };
    assert_eq!(
        super::weather_effect_copy(effects),
        "cave-in risk eases, ore veins are thin, seams glitter, ground is soft, the deep stirs, relics surface more often, darkness drains lanterns quickly"
    );
    assert_eq!(
        super::weather_effect_copy(cama_app::dig_runtime::DigWeatherEffects::default()),
        "no notable effect"
    );
}

#[test]
fn paid_prompt_is_tokenized_owner_bound_one_shot_and_exactly_formatted() {
    let (_database, provider, _discord) = fixture();
    let token = provider
        .handler
        .create_paid_view(USER as i64, GUILD as i64, 100)
        .expect("create paid view");
    assert_eq!(
        provider
            .handler
            .claim_paid_view(&token, USER as i64 + 1, GUILD as i64, 159)
            .expect("wrong owner admission"),
        super::DigPaidViewAdmission::WrongOwner
    );
    assert_eq!(
        provider
            .handler
            .claim_paid_view(&token, USER as i64, GUILD as i64, 159)
            .expect("owner claim"),
        super::DigPaidViewAdmission::Admitted
    );
    assert_eq!(
        provider
            .handler
            .claim_paid_view(&token, USER as i64, GUILD as i64, 159)
            .expect("duplicate claim"),
        super::DigPaidViewAdmission::AlreadyClaimed
    );
    let expired = provider
        .handler
        .create_paid_view(USER as i64, GUILD as i64, 200)
        .expect("create expiring paid view");
    assert_eq!(
        provider
            .handler
            .claim_paid_view(&expired, USER as i64, GUILD as i64, 260)
            .expect("inclusive expiry"),
        super::DigPaidViewAdmission::Expired
    );
    let (_restarted_database, restarted, _discord) = fixture();
    assert_eq!(
        restarted
            .handler
            .claim_paid_view(&token, USER as i64, GUILD as i64, 101)
            .expect("restart invalidation"),
        super::DigPaidViewAdmission::Expired
    );

    let result = DigRuntimeOutcome {
        success: false,
        error: Some("cooldown".to_owned()),
        depth_before: 10,
        depth_after: 10,
        advance: 0,
        jc_earned: 0,
        vanity_tax: 0,
        low_priority_tax: 0,
        balance_after: 100,
        tunnel_name: "Test Tunnel".to_owned(),
        milestone_bonus: 0,
        streak_bonus: 0,
        bankruptcy_penalty: 0,
        luminosity_after: 100,
        luminosity_drained: 0,
        corruption_description: None,
        mutation_names: Vec::new(),
        tip: String::new(),
        cave_in: false,
        cave_in_detail: None,
        event_id: None,
        artifact_id: None,
        boss_boundary: None,
        first_dig: false,
        paid_dig_cost: 25,
        cooldown_remaining: 3_599,
        paid_dig_available: true,
        items_used: Vec::new(),
        consumed_item_ids: Vec::new(),
        action_id: None,
        route_choice_required: false,
        pickaxe_tier: 0,
        pet_dig_bonus: 0,
        pet_name: None,
        forced_event_consumed: false,
        relic_trim_notice: false,
        weather: None,
    };
    let response = super::paid_dig_response(&result, "secret-token");
    assert_eq!(response.embeds[0].color, Some(0xFF_A5_00));
    let expected = format!(
        "Free dig on cooldown for **59m 59s**.\nContinuing costs **25** {JOPACOIN_EMOTE}. Proceed?"
    );
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        response.components[0].buttons[0].custom_id,
        "dig:paid:confirm:secret-token"
    );
    assert_eq!(
        response.components[0].buttons[1].custom_id,
        "dig:paid:cancel:secret-token"
    );
    assert!(
        !response.components[0].buttons[0]
            .custom_id
            .contains(&USER.to_string())
    );
}

#[tokio::test]
async fn paid_confirmation_recovers_pending_dig_without_committing_another() {
    let (database, provider, _discord) = fixture();
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0xdec0_1001,
                CHANNEL as i64,
                "Pending Miner",
                None,
            ),
        )
        .await
        .expect("commit pending Dig");
    assert!(execution.delivery.is_some());
    let before = DigRuntimeService::sqlite(database.path())
        .snapshot(USER as i64, GUILD as i64)
        .expect("snapshot before paid confirmation");
    let token = provider
        .handler
        .create_paid_view(USER as i64, GUILD as i64, super::unix_now())
        .expect("create paid view");
    let responder = Arc::new(TestResponder::default());

    provider
        .handler
        .handle(
            component_request(format!("dig:paid:confirm:{token}"), Vec::new()),
            responder.clone(),
        )
        .await
        .expect("recover pending Dig instead of charging");

    let after = DigRuntimeService::sqlite(database.path())
        .snapshot(USER as i64, GUILD as i64)
        .expect("snapshot after paid confirmation");
    assert_eq!(after.balance, before.balance);
    assert_eq!(
        after.tunnel.as_ref().map(|tunnel| tunnel.total_digs),
        before.tunnel.as_ref().map(|tunnel| tunnel.total_digs)
    );
    assert!(
        responder
            .original_edits
            .lock()
            .expect("paid recovery edit")
            .iter()
            .any(|response| response
                .content
                .contains("you were not charged for another dig"))
    );
}

#[tokio::test]
async fn missing_flavor_receipt_falls_back_to_terminal_delivery() {
    let (database, provider, _discord) = fixture();
    let execution = provider
        .handler
        .run_dig(
            USER as i64,
            GUILD as i64,
            super::unix_now(),
            false,
            false,
            cama_app::dig_runtime::DigRuntimeDeliveryContext::new(
                0xdec0_1002,
                CHANNEL as i64,
                "Flavorless Miner",
                None,
            ),
        )
        .await
        .expect("commit Dig before flavor failure");
    let pending = execution.delivery.expect("pending delivery");
    Connection::open(database.path())
        .expect("open flavor failure fixture")
        .execute_batch(
            "CREATE TRIGGER reject_flavor_receipt
             BEFORE INSERT ON dig_actions
             WHEN NEW.action_type='dig_flavor_bonus'
             BEGIN
               SELECT RAISE(ABORT, 'injected flavor receipt failure');
             END;",
        )
        .expect("install flavor receipt failure");

    let prepared = provider
        .handler
        .prepare_delivery(&pending)
        .await
        .expect("missing flavor receipt is fail-soft");

    assert_eq!(prepared.flavor, DigRuntimeFlavorSnapshot::Skipped);
    assert!(prepared.blood_pact.is_terminal());
}

#[tokio::test(start_paused = true)]
async fn unresolved_paid_prompt_is_actively_cancelled_at_timeout() {
    let (_database, provider, _discord) = fixture();
    let responder = Arc::new(TestResponder::default());
    let token = provider
        .handler
        .create_paid_view(USER as i64, GUILD as i64, 100)
        .expect("create paid view");
    provider.handler.schedule_paid_view_timeout(
        token.clone(),
        responder.clone(),
        Some(InteractionMessageReceipt {
            message_id: 101,
            channel_id: CHANNEL,
            delivery: crate::registration::InteractionMessageDelivery::InteractionFollowup,
        }),
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    let edits = responder.message_edits.lock().expect("timeout edits");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].1.content, "Dig cancelled.");
    assert!(edits[0].1.components.is_empty());
    drop(edits);
    assert_eq!(
        provider
            .handler
            .claim_paid_view(&token, USER as i64, GUILD as i64, 160)
            .expect("retired timeout view"),
        super::DigPaidViewAdmission::Expired
    );
}

#[tokio::test]
async fn admin_depth_and_cooldown_commands_preserve_max_and_report_missing_tunnel() {
    let (database, provider, _discord) = fixture();
    Connection::open(database.path())
        .expect("open migrated database")
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,last_dig_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                USER as i64,
                GUILD as i64,
                75_i64,
                275_i64,
                1_700_000_000_i64
            ],
        )
        .expect("seed admin target tunnel");
    let set_depth = Arc::new(TestResponder::default());
    provider
        .handler
        .command_admin(
            USER as i64,
            GUILD as i64,
            Some(1_u64 << 3),
            "setdepth",
            &[
                InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: USER,
                        display_name: Some("Dig Test Miner".to_owned()),
                        is_bot: Some(false),
                    },
                },
                InteractionOption {
                    name: "depth".to_owned(),
                    value: InteractionValue::Integer(12),
                },
            ],
            set_depth.clone(),
        )
        .await
        .expect("set depth command");
    assert_eq!(set_depth.defers.lock().unwrap().as_slice(), &[true]);
    assert_eq!(
        set_depth.followups.lock().unwrap()[0].content,
        format!("Set <@{USER}> to depth **12** and reset cooldown.")
    );
    let stored = Connection::open(database.path())
        .expect("reopen migrated database")
        .query_row(
            "SELECT depth,max_depth,last_dig_at FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("read admin target");
    assert_eq!(stored, (12, 275, 0));

    let missing = Arc::new(TestResponder::default());
    provider
        .handler
        .command_admin(
            USER as i64,
            GUILD as i64,
            Some(1_u64 << 3),
            "resetcooldown",
            &[InteractionOption {
                name: "user".to_owned(),
                value: InteractionValue::User {
                    id: USER + 1,
                    display_name: Some("Missing Miner".to_owned()),
                    is_bot: Some(false),
                },
            }],
            missing.clone(),
        )
        .await
        .expect("missing cooldown target");
    assert_eq!(
        missing.followups.lock().unwrap()[0].content,
        "That player doesn't have a tunnel."
    );
}

#[tokio::test]
async fn prestige_preview_selection_restart_and_forgery_use_atomic_app_service() {
    let (database, provider, discord) = fixture();
    let mut progress = serde_json::Map::new();
    for boundary in cama_app::dig_bosses::BOSS_BOUNDARIES {
        progress.insert(
            boundary.to_string(),
            serde_json::Value::String("defeated".to_owned()),
        );
    }
    progress.insert(
        cama_app::dig_bosses::PINNACLE_DEPTH.to_string(),
        serde_json::json!({"status": "defeated", "boss_id": "forgotten_king"}),
    );
    Connection::open(database.path())
        .expect("prestige fixture DB")
        .execute(
            "INSERT INTO tunnels (
                 discord_id,guild_id,depth,max_depth,prestige_level,
                 prestige_perks,boss_progress,current_run_jc,current_run_events,
                 pickaxe_tier,tunnel_name
             ) VALUES (?1,?2,350,350,0,'[]',?3,50,7,4,'Prestige Tunnel')",
            params![
                USER as i64,
                GUILD as i64,
                serde_json::Value::Object(progress).to_string(),
            ],
        )
        .expect("seed prestige tunnel");

    let preview_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(50, "prestige", Vec::new()),
            preview_responder.clone(),
        )
        .await
        .expect("prestige preview");
    let preview = preview_responder
        .responses
        .lock()
        .expect("preview responses")[0]
        .clone();
    assert!(preview.ephemeral);
    assert!(
        preview.embeds[0]
            .title
            .as_deref()
            .is_some_and(|title| title.contains("Prestige to P1"))
    );
    assert_eq!(preview.components[0].buttons.len(), 4);
    let chosen = preview.components[0].buttons[0].custom_id.clone();

    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            USER as i64 + 1,
            "prestige-copy",
            Some(GUILD as i64),
        ))
        .expect("register copied-view actor");
    let copied = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request_as(USER + 1, "Copy", &chosen, Vec::new()),
            copied.clone(),
        )
        .await
        .expect("copied prestige rejected");
    assert!(
        copied.responses.lock().expect("copied responses")[0]
            .content
            .contains("isn't your prestige")
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        Arc::new(TestDiscord::default()),
        None,
        Arc::clone(&provider.handler.state.media),
    );
    let expired = Arc::new(TestResponder::default());
    restarted
        .handler
        .handle(component_request(&chosen, Vec::new()), expired.clone())
        .await
        .expect("old process nonce expires");
    assert!(
        expired.responses.lock().expect("expired responses")[0]
            .content
            .contains("expired")
    );

    let forged_perk = cama_app::dig_tunnels::PRESTIGE_PERKS
        .iter()
        .find(|perk| {
            !preview.components[0]
                .buttons
                .iter()
                .any(|button| button.custom_id.contains(**perk))
        })
        .expect("an unoffered catalog perk");
    let mut forged_parts = chosen.split(':').map(str::to_owned).collect::<Vec<_>>();
    let perk_index = forged_parts.len() - 2;
    forged_parts[perk_index] = (*forged_perk).to_owned();
    let forged = forged_parts.join(":");
    let forged_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(forged, Vec::new()),
            forged_responder.clone(),
        )
        .await
        .expect("forged offered subset rejected");
    assert!(
        forged_responder.updates.lock().expect("forged updates")[0]
            .content
            .contains("Invalid perk")
    );

    let committed = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(component_request(chosen, Vec::new()), committed.clone())
        .await
        .expect("prestige selection commits");
    let result = committed.updates.lock().expect("prestige update")[0].clone();
    assert!(result.components.is_empty());
    assert_eq!(
        result.embeds[0].title.as_deref(),
        Some("Prestige 1 Complete!")
    );
    {
        let public = discord.public.lock().expect("public ascension responses");
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].content, "*dig-test-miner has ascended.*");
        assert!(
            public[0].attachments.is_empty(),
            "test config disables Neon"
        );
    }
    let connection = Connection::open(database.path()).expect("verify prestige DB");
    let (depth, level, pickaxe, balance, audits, relics): (i64, i64, i64, i64, i64, i64) =
        connection
            .query_row(
                "SELECT CAST(t.depth AS INTEGER), CAST(t.prestige_level AS INTEGER),
                        CAST(t.pickaxe_tier AS INTEGER), p.jopacoin_balance,
                        (SELECT COUNT(*) FROM dig_actions a
                          WHERE a.actor_id=t.discord_id AND a.guild_id=t.guild_id
                            AND a.action_type='prestige'),
                        (SELECT COUNT(*) FROM dig_artifacts r
                          WHERE r.discord_id=t.discord_id AND r.guild_id=t.guild_id)
                   FROM tunnels t JOIN players p
                     ON p.discord_id=t.discord_id AND p.guild_id=t.guild_id
                  WHERE t.discord_id=?1 AND t.guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("prestige state");
    assert_eq!((depth, level, pickaxe), (0, 1, 4));
    assert_eq!(balance, 1_150);
    assert_eq!(audits, 1);
    assert_eq!(relics, 1);

    let duplicate = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                preview.components[0].buttons[0].custom_id.clone(),
                Vec::new(),
            ),
            duplicate.clone(),
        )
        .await
        .expect("duplicate delivery is harmless");
    assert!(
        duplicate.responses.lock().expect("duplicate response")[0]
            .content
            .contains("already made your selection")
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT jopacoin_balance FROM players
                  WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1_150
    );
}

#[tokio::test]
async fn live_prestige_provider_attaches_pinnacle_gif_through_temporary_send() {
    let config = ApplicationConfig::from_lookup(|name| match name {
        "DISCORD_BOT_TOKEN" => Some("dig-provider-neon-test-token".to_owned()),
        "NEON_DEGEN_ENABLED" => Some("true".to_owned()),
        _ => None,
    })
    .expect("Neon-enabled provider test config");
    let (database, provider, discord) =
        fixture_with_discord_and_config(Arc::new(TestDiscord::default()), config);
    let mut progress = serde_json::Map::new();
    for boundary in cama_app::dig_bosses::BOSS_BOUNDARIES {
        progress.insert(
            boundary.to_string(),
            serde_json::Value::String("defeated".to_owned()),
        );
    }
    progress.insert(
        cama_app::dig_bosses::PINNACLE_DEPTH.to_string(),
        serde_json::json!({"status": "defeated", "boss_id": "forgotten_king"}),
    );
    Connection::open(database.path())
        .expect("live Neon fixture DB")
        .execute(
            "INSERT INTO tunnels (
                 discord_id,guild_id,depth,max_depth,prestige_level,
                 prestige_perks,boss_progress,current_run_jc,current_run_events,
                 pickaxe_tier,tunnel_name
             ) VALUES (?1,?2,350,350,0,'[]',?3,50,7,4,'Prestige Tunnel')",
            params![
                USER as i64,
                GUILD as i64,
                serde_json::Value::Object(progress).to_string(),
            ],
        )
        .expect("seed live Neon prestige tunnel");

    // Make the service's production provider call deterministic while
    // retaining the real SeededDigNeonRandom and cooldown implementation.
    {
        let mut neon = provider
            .handler
            .state
            .neon
            .lock()
            .expect("Neon service lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
    }

    let preview_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(60, "prestige", Vec::new()),
            preview_responder.clone(),
        )
        .await
        .expect("live Neon prestige preview");
    let chosen = preview_responder
        .responses
        .lock()
        .expect("live Neon preview response")[0]
        .components[0]
        .buttons[0]
        .custom_id
        .clone();
    let committed = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(component_request(chosen, Vec::new()), committed.clone())
        .await
        .expect("live Neon prestige selection");

    // The component handler above is the real production path:
    // prestige_neon_response -> dig_neon_response ->
    // DigDiscordPort::dig_send_temporary. TestDiscord retains temporary
    // messages, allowing the attachment and lifecycle boundary to be
    // asserted without a Discord network dependency.
    let public = discord.public.lock().expect("live Neon public responses");
    let neon_response = public
        .iter()
        .find(|response| !response.attachments.is_empty())
        .expect("live prestige path sends a Neon attachment");
    assert_eq!(neon_response.attachments.len(), 1);
    let attachment = &neon_response.attachments[0];
    assert_eq!(attachment.filename, "jopat_terminal.gif");
    let media = cama_app::dig_assets::inspect_media(&attachment.bytes)
        .expect("live provider attachment is valid media");
    assert_eq!(media.format, cama_app::dig_assets::MediaFormat::Gif);
    assert_eq!((media.width, media.height), (320, 180));
    assert_eq!(media.frame_count, 30);
}

#[tokio::test]
async fn boss_neon_live_transport_is_time_limited_and_idempotent() {
    let lifecycle = Arc::new(StdMutex::new(Vec::new()));
    let discord = Arc::new(TestDiscord::default().with_lifecycle(Arc::clone(&lifecycle)));
    let (database, provider, discord) =
        fixture_with_discord_and_config(discord, neon_config("0.30"));
    {
        let mut neon = provider
            .handler
            .state
            .neon
            .lock()
            .expect("Neon service lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
    }

    // The real provider transport path is used here; the primary result
    // follow-up is accepted before the best-effort temporary Neon hook.
    let responder = TestResponder::with_lifecycle(Arc::clone(&lifecycle));
    responder
        .followup(InteractionResponse::message("primary boss result"))
        .await
        .expect("primary follow-up");
    provider
        .handler
        .post_boss_neon(
            Some(8801),
            USER as i64,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(DigBossNeonVictory {
                boss_name: "Grothak".to_owned(),
                boundary: 100,
                layer_name: "Stone".to_owned(),
                jc_delta: 500,
                gear_drop: false,
                trophy_relic_drop: false,
            }),
        )
        .await;
    assert_eq!(
        *lifecycle.lock().expect("lifecycle log"),
        vec!["followup", "temporary"]
    );
    {
        let temporary = discord.temporary.lock().expect("temporary Neon sends");
        assert_eq!(temporary.len(), 1);
        assert_eq!(temporary[0].0, CHANNEL as i64);
        assert_eq!(temporary[0].1, Duration::from_secs(60));
        assert_eq!(temporary[0].2.attachments.len(), 1);
    }

    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            USER as i64 + 1,
            "dig-pinnacle-neon-miner",
            Some(GUILD as i64),
        ))
        .expect("register Pinnacle Neon miner");
    {
        let mut neon = provider
            .handler
            .state
            .neon
            .lock()
            .expect("Pinnacle Neon lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
    }
    provider
        .handler
        .post_boss_neon(
            Some(8804),
            USER as i64 + 1,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(DigBossNeonVictory {
                boss_name: "The Crowned Hunger".to_owned(),
                boundary: cama_app::dig_bosses::PINNACLE_DEPTH.into(),
                layer_name: "The Pinnacle".to_owned(),
                jc_delta: 500,
                gear_drop: false,
                trophy_relic_drop: false,
            }),
        )
        .await;
    {
        let temporary = discord.temporary.lock().expect("Pinnacle temporary send");
        assert_eq!(temporary.len(), 2);
        let pinnacle_media =
            cama_app::dig_assets::inspect_media(&temporary[1].2.attachments[0].bytes)
                .expect("Pinnacle Neon GIF");
        assert_eq!((pinnacle_media.width, pinnacle_media.height), (320, 180));
    }

    // A retry of the same resolved action is suppressed by the durable
    // one-time claim, even though the provider process is still alive.
    provider
        .handler
        .post_boss_neon(
            Some(8801),
            USER as i64,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(DigBossNeonVictory {
                boss_name: "Grothak".to_owned(),
                boundary: 100,
                layer_name: "Stone".to_owned(),
                jc_delta: 500,
                gear_drop: false,
                trophy_relic_drop: false,
            }),
        )
        .await;
    assert_eq!(
        discord
            .temporary
            .lock()
            .expect("duplicate Neon sends")
            .len(),
        2
    );
    assert_eq!(
        Connection::open(database.path())
            .expect("Neon claim database")
            .query_row(
                "SELECT COUNT(*) FROM neon_events
                 WHERE discord_id=?1 AND guild_id=?2 AND event_type=?3",
                params![USER as i64, GUILD as i64, "dig:8801:boss_neon"],
                |row| row.get::<_, i64>(0),
            )
            .expect("boss Neon claim"),
        1
    );
}

#[tokio::test]
async fn boss_neon_miss_and_delivery_failure_remain_terminal() {
    let discord = Arc::new(TestDiscord::default());
    let (_database, provider, discord) =
        fixture_with_discord_and_config(discord, neon_config("0.0"));
    {
        let mut neon = provider
            .handler
            .state
            .neon
            .lock()
            .expect("Neon service lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(0x1234_5678_9abc_def0);
    }
    let victory = DigBossNeonVictory {
        boss_name: "Grothak".to_owned(),
        boundary: 100,
        layer_name: "Stone".to_owned(),
        jc_delta: 500,
        gear_drop: false,
        trophy_relic_drop: false,
    };
    provider
        .handler
        .post_boss_neon(
            Some(8802),
            USER as i64,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(victory.clone()),
        )
        .await;
    assert!(discord.temporary.lock().expect("miss sends").is_empty());
    {
        let mut neon = provider
            .handler
            .state
            .neon
            .lock()
            .expect("Neon service lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
    }
    provider
        .handler
        .post_boss_neon(
            Some(8802),
            USER as i64,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(victory.clone()),
        )
        .await;
    assert!(
        discord
            .temporary
            .lock()
            .expect("miss retry sends")
            .is_empty(),
        "miss claim prevents a retry reroll"
    );

    let failed_discord = Arc::new(TestDiscord::default());
    failed_discord.reject_un_nonnced_public_send();
    let (failed_database, failed_provider, failed_discord) =
        fixture_with_discord_and_config(failed_discord, neon_config("0.30"));
    {
        let mut neon = failed_provider
            .handler
            .state
            .neon
            .lock()
            .expect("failed Neon service lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
    }
    failed_provider
        .handler
        .post_boss_neon(
            Some(8803),
            USER as i64,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(victory.clone()),
        )
        .await;
    failed_discord.allow_un_nonnced_public_send();
    failed_provider
        .handler
        .post_boss_neon(
            Some(8803),
            USER as i64,
            GUILD as i64,
            Some(CHANNEL as i64),
            Some(victory),
        )
        .await;
    assert!(
        failed_discord
            .temporary
            .lock()
            .expect("failed retry sends")
            .is_empty(),
        "delivery failure remains at-most-once"
    );
    assert_eq!(
        Connection::open(failed_database.path())
            .expect("failed Neon claim database")
            .query_row(
                "SELECT COUNT(*) FROM neon_events
                 WHERE discord_id=?1 AND guild_id=?2 AND event_type=?3",
                params![USER as i64, GUILD as i64, "dig:8803:boss_neon"],
                |row| row.get::<_, i64>(0),
            )
            .expect("failed boss Neon claim"),
        1
    );
}

#[tokio::test]
async fn boss_modal_and_resume_live_path_sends_neon_after_primary_result() {
    let lifecycle = Arc::new(StdMutex::new(Vec::new()));
    let discord = Arc::new(TestDiscord::default().with_lifecycle(Arc::clone(&lifecycle)));
    let (database, provider, discord) =
        fixture_with_discord_and_config(discord, neon_config("0.30"));
    Connection::open(database.path())
        .expect("live boss fixture DB")
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,boss_progress,
              boss_attempts,last_dig_at,luminosity,stat_points,tunnel_name)
             VALUES (?1,?2,24,24,0,?3,0,0,50,5,'Live Neon Boss')",
            params![
                USER as i64,
                GUILD as i64,
                r#"{"25":{"boss_id":"grothak","status":"active","hp_remaining":1,"hp_max":4}}"#,
            ],
        )
        .expect("live boss tunnel");
    provider.handler.state.boss_entropy.reseed(1);
    {
        let mut neon = provider.handler.state.neon.lock().expect("live Neon lock");
        *neon.random_mut() = cama_app::dig_neon::SeededDigNeonRandom::new(1);
    }

    let mut response = Arc::new(TestResponder::with_lifecycle(Arc::clone(&lifecycle)));
    provider
        .handler
        .handle(
            modal_request(
                format!("dig:boss:wager:{USER}:{GUILD}"),
                [("risk_tier", "cautious"), ("wager", "0")],
            ),
            response.clone(),
        )
        .await
        .expect("live boss modal path");

    // Resolve any authored mechanic prompt through its actual namespaced
    // component route. A terminal result is the only result allowed to
    // reach the Neon side effect.
    for _ in 0..4 {
        let next = response
            .followups
            .lock()
            .expect("boss followups")
            .iter()
            .flat_map(|followup| &followup.components)
            .flat_map(|row| &row.buttons)
            .find(|button| button.custom_id.starts_with("dig:boss:duel:"))
            .map(|button| button.custom_id.clone());
        let Some(next) = next else {
            break;
        };
        response = Arc::new(TestResponder::with_lifecycle(Arc::clone(&lifecycle)));
        provider
            .handler
            .handle(component_request(next, Vec::new()), response.clone())
            .await
            .expect("live boss resume path");
    }

    let temporary = discord.temporary.lock().expect("live boss Neon sends");
    assert_eq!(temporary.len(), 1, "only terminal win emits Neon");
    assert_eq!(temporary[0].1, Duration::from_secs(60));
    assert_eq!(temporary[0].2.attachments.len(), 1);
    let log = lifecycle.lock().expect("live lifecycle ordering");
    let primary = log.iter().position(|event| *event == "followup");
    let neon = log.iter().position(|event| *event == "temporary");
    assert!(primary.is_some_and(|index| neon.is_some_and(|neon| index < neon)));
}

#[test]
fn prestige_view_owner_timeout_transition_and_claim_are_exact() {
    let (_database, provider, _discord) = fixture();
    let token = provider
        .handler
        .create_prestige_view(USER as i64, GUILD as i64, false, 100)
        .expect("create perk-only view");
    assert_eq!(
        provider
            .handler
            .inspect_prestige_view(&token, USER as i64, GUILD as i64, 159)
            .unwrap(),
        DigPrestigeViewAdmission::Admitted
    );
    assert_eq!(
        provider
            .handler
            .inspect_prestige_view(&token, USER as i64 + 1, GUILD as i64, 159)
            .unwrap(),
        DigPrestigeViewAdmission::WrongOwner
    );
    assert_eq!(
        provider
            .handler
            .inspect_prestige_view(&token, USER as i64, GUILD as i64, 160)
            .unwrap(),
        DigPrestigeViewAdmission::Expired
    );

    let mutation_token = provider
        .handler
        .create_prestige_view(USER as i64, GUILD as i64, true, 200)
        .expect("create mutation view");
    assert_eq!(
        provider
            .handler
            .claim_prestige_view(
                &mutation_token,
                USER as i64,
                GUILD as i64,
                Some("fractured_vein"),
                200,
            )
            .unwrap(),
        DigPrestigeViewAdmission::InvalidTransition
    );
    assert_eq!(
        provider
            .handler
            .select_prestige_mutation(
                &mutation_token,
                USER as i64,
                GUILD as i64,
                "fractured_vein",
                200,
            )
            .unwrap(),
        DigPrestigeViewAdmission::Admitted
    );
    assert_eq!(
        provider
            .handler
            .select_prestige_mutation(
                &mutation_token,
                USER as i64,
                GUILD as i64,
                "volatile_depths",
                200,
            )
            .unwrap(),
        DigPrestigeViewAdmission::AlreadyClaimed
    );
    assert_eq!(
        provider
            .handler
            .claim_prestige_view(
                &mutation_token,
                USER as i64,
                GUILD as i64,
                Some("volatile_depths"),
                200,
            )
            .unwrap(),
        DigPrestigeViewAdmission::InvalidTransition
    );
    assert_eq!(
        provider
            .handler
            .claim_prestige_view(
                &mutation_token,
                USER as i64,
                GUILD as i64,
                Some("fractured_vein"),
                200,
            )
            .unwrap(),
        DigPrestigeViewAdmission::Admitted
    );
    assert_eq!(
        provider
            .handler
            .claim_prestige_view(
                &mutation_token,
                USER as i64,
                GUILD as i64,
                Some("fractured_vein"),
                200,
            )
            .unwrap(),
        DigPrestigeViewAdmission::AlreadyClaimed
    );
}

#[test]
fn rendered_media_keeps_stats_and_event_ui_separate_with_exact_attachments() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|ancestor| ancestor.join("assets/dig"))
        .find(|candidate| candidate.is_dir())
        .expect("repository Dig asset tree");
    let media = DigMediaRuntime::production(&DigRuntimeConfig::with_asset_root(root));
    let result = DigRuntimeOutcome {
        success: true,
        error: None,
        depth_before: 20,
        depth_after: 24,
        advance: 4,
        jc_earned: 6,
        vanity_tax: 0,
        low_priority_tax: 0,
        balance_after: 106,
        tunnel_name: "The Media Mine".to_owned(),
        milestone_bonus: 0,
        streak_bonus: 3,
        bankruptcy_penalty: 0,
        luminosity_after: 90,
        luminosity_drained: 10,
        corruption_description: Some("-1 JC this dig".to_owned()),
        mutation_names: vec!["Dark Sight".to_owned()],
        tip: "Keep digging!".to_owned(),
        cave_in: false,
        cave_in_detail: None,
        event_id: Some("underground_stream".to_owned()),
        artifact_id: None,
        boss_boundary: None,
        first_dig: false,
        paid_dig_cost: 0,
        cooldown_remaining: 0,
        paid_dig_available: false,
        items_used: vec!["torch".to_owned(), "dynamite".to_owned()],
        consumed_item_ids: vec![1, 2],
        action_id: Some(91),
        route_choice_required: false,
        pickaxe_tier: 0,
        pet_dig_bonus: 0,
        pet_name: None,
        forced_event_consumed: false,
        relic_trim_notice: false,
        weather: Some(cama_app::dig_runtime::DigRuntimeWeatherInfo {
            name: "Earthworm Migration".to_owned(),
            description: "Worms churn the soil. Digging is easy, but they ate all the coins."
                .to_owned(),
        }),
    };

    let (stats, event) =
        super::dig_responses(&result, "Media Miner", None, &media, "test-nonce", None);
    let event = event.expect("choice event UI");
    assert!(
        stats.components.is_empty(),
        "stats card has no event controls"
    );
    assert_eq!(stats.attachments.len(), 3);
    assert!(
        stats
            .attachments
            .iter()
            .any(|file| file.filename == "items_used.png")
    );
    assert!(
        stats.embeds[0]
            .thumbnail_url
            .as_deref()
            .is_some_and(|url| url.starts_with("attachment://layer_"))
    );
    assert_eq!(
        stats.embeds[0].title.as_deref(),
        Some("The Media Mine — Depth 24")
    );
    assert!(
        stats.embeds[0]
            .fields
            .iter()
            .any(|field| { field.name == "Progress" && field.value.starts_with("+4 blocks | +6") })
    );
    assert!(stats.embeds[0].fields.iter().any(|field| {
        field.name == "Luminosity" && field.value == "`[█████████░]` 90% — Bright (-10)"
    }));
    assert!(
        stats.embeds[0]
            .fields
            .iter()
            .any(|field| field.name == "Corruption" && field.value == "-1 JC this dig")
    );
    assert!(
        !stats.embeds[0]
            .fields
            .iter()
            .any(|field| field.name == "Weather" || field.name == "Depth" || field.name == "Layer")
    );
    assert_eq!(
        stats.embeds[0].footer.as_deref(),
        Some("Mutations: Dark Sight | Keep digging!")
    );
    assert_eq!(
        stats.embeds[0].footer_icon_url.as_deref(),
        Some("attachment://pickaxe_wooden.png")
    );
    assert_eq!(
        stats.embeds[0].image_url.as_deref(),
        Some("attachment://items_used.png")
    );
    assert_eq!(event.attachments.len(), 1);
    assert_eq!(event.components.len(), 1);
    assert_eq!(event.components[0].buttons.len(), 2);
    assert!(event.components[0].buttons.iter().all(|button| {
        button
            .custom_id
            .starts_with("dig:event-action:test-nonce:91:")
    }));

    let (retry_stats, retry_event) =
        super::dig_responses(&result, "Media Miner", None, &media, "test-nonce", None);
    assert_eq!(stats.attachments, retry_stats.attachments);
    assert_eq!(
        event.attachments,
        retry_event.expect("retry event").attachments
    );
}

#[test]
fn event_prompt_applies_durable_darkness_and_reading_the_stone_policy() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|ancestor| ancestor.join("assets/dig"))
        .find(|candidate| candidate.is_dir())
        .expect("repository Dig asset tree");
    let media = DigMediaRuntime::production(&DigRuntimeConfig::with_asset_root(root));
    let result = DigRuntimeOutcome {
        success: true,
        error: None,
        depth_before: 20,
        depth_after: 24,
        advance: 4,
        jc_earned: 6,
        vanity_tax: 0,
        low_priority_tax: 0,
        balance_after: 106,
        tunnel_name: "The Event Mine".to_owned(),
        milestone_bonus: 0,
        streak_bonus: 0,
        bankruptcy_penalty: 0,
        luminosity_after: 0,
        luminosity_drained: 10,
        corruption_description: None,
        mutation_names: Vec::new(),
        tip: "Keep digging!".to_owned(),
        cave_in: false,
        cave_in_detail: None,
        event_id: Some("underground_stream".to_owned()),
        artifact_id: None,
        boss_boundary: None,
        first_dig: false,
        paid_dig_cost: 0,
        cooldown_remaining: 0,
        paid_dig_available: false,
        items_used: Vec::new(),
        consumed_item_ids: Vec::new(),
        action_id: Some(92),
        route_choice_required: false,
        pickaxe_tier: 0,
        pet_dig_bonus: 0,
        pet_name: None,
        forced_event_consumed: false,
        relic_trim_notice: false,
        weather: None,
    };
    let prompt = cama_app::dig_event_runtime::DigEventActionPresentation {
        event: cama_app::dig_loot::canonical_event_presentation("underground_stream")
            .expect("canonical prompt"),
        luminosity: 0,
        safe_disabled: true,
        reading_the_stone_hint: Some("The stones hum louder beside the bolder path.".to_owned()),
    };

    let (_, event) = super::dig_responses(
        &result,
        "Dark Miner",
        None,
        &media,
        "test-nonce",
        Some(&prompt),
    );
    let event = event.expect("event prompt");
    assert!(event.embeds[0].fields.iter().any(|field| {
        field
            .value
            .contains("The stones hum louder beside the bolder path.")
    }));
    let safe = event.components[0]
        .buttons
        .iter()
        .find(|button| button.custom_id.ends_with(":safe"))
        .expect("safe button");
    assert_eq!(safe.label, "Darkness consumes safety");
    assert!(safe.disabled);
}

#[tokio::test]
async fn shop_buy_and_inventory_attach_the_canonical_media_contract() {
    let (_database, provider, _discord) = fixture();
    let go = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(go_request(), go)
        .await
        .expect("first dig creates a tunnel");

    let shop = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(command_request(20, "shop", Vec::new()), shop.clone())
        .await
        .expect("shop renders");
    {
        let shop_responses = shop.followups.lock().expect("shop responses");
        assert_eq!(shop_responses.len(), 1);
        assert_eq!(shop_responses[0].attachments.len(), 1);
        assert_eq!(shop_responses[0].attachments[0].filename, "shop_grid.png");
        assert_eq!(
            shop_responses[0].embeds[0].image_url.as_deref(),
            Some("attachment://shop_grid.png")
        );
    }

    let buy = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(
                21,
                "buy",
                vec![InteractionOption {
                    name: "item".to_owned(),
                    value: InteractionValue::String("torch".to_owned()),
                }],
            ),
            buy.clone(),
        )
        .await
        .expect("buy renders");
    {
        let buy_responses = buy.followups.lock().expect("buy responses");
        assert_eq!(buy_responses.len(), 1);
        assert!(buy_responses[0].ephemeral);
        assert_eq!(buy_responses[0].attachments.len(), 1);
        assert!(
            buy_responses[0].attachments[0]
                .filename
                .starts_with("item_torch.")
        );
        assert!(
            buy_responses[0].embeds[0]
                .thumbnail_url
                .as_deref()
                .is_some_and(|url| url.starts_with("attachment://item_torch."))
        );
    }

    let inventory = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(22, "inventory", Vec::new()),
            inventory.clone(),
        )
        .await
        .expect("inventory renders");
    let inventory_responses = inventory.followups.lock().expect("inventory responses");
    assert_eq!(inventory_responses.len(), 1);
    assert_eq!(inventory_responses[0].attachments.len(), 1);
    assert_eq!(
        inventory_responses[0].attachments[0].filename,
        "pickaxe_wooden.png"
    );
    assert_eq!(
        inventory_responses[0].embeds[0].thumbnail_url.as_deref(),
        Some("attachment://pickaxe_wooden.png")
    );
    assert_eq!(inventory_responses[0].embeds[0].color, Some(0x8B_45_13));
    assert_eq!(inventory_responses[0].embeds[0].fields.len(), 1);
    assert_eq!(
        inventory_responses[0].embeds[0].fields[0].name,
        "Torch [QUEUED]"
    );
    assert_eq!(
        inventory_responses[0].embeds[0].fields[0].value,
        "+50 luminosity. Light the way."
    );
}

// tests/test_dig_shop.py::test_dig_shop_handler_falls_back_to_ephemeral_when_public_send_fails
#[tokio::test]
async fn shop_public_send_falls_back_to_an_ephemeral_response() {
    let (_database, provider, _discord) = fixture();
    let responder = Arc::new(RejectingPublicFollowupResponder::default());
    provider
        .handler
        .handle(command_request(23, "shop", Vec::new()), responder.clone())
        .await
        .expect("ephemeral fallback");
    assert_eq!(responder.defers.lock().unwrap().as_slice(), &[false]);
    let attempts = responder.attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(!attempts[0].ephemeral);
    assert!(attempts[1].ephemeral);
    assert_eq!(attempts[0].embeds, attempts[1].embeds);
    assert_eq!(attempts[0].attachments, attempts[1].attachments);
}

// tests/test_dig_shop.py::test_shop_consumables_field_fits_discord_limit
#[tokio::test]
async fn shop_consumables_fields_stay_within_discord_limit_without_dropping_rows() {
    let (database, provider, _discord) = fixture();
    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(command_request(230, "shop", Vec::new()), responder.clone())
        .await
        .expect("shop field rendering");
    let attempts = responder.followups.lock().unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(
        attempts[0].embeds[0]
            .fields
            .iter()
            .all(|field| field.value.len() <= 1_024)
    );

    let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database.path())
        .shop(USER as i64, GUILD as i64)
        .expect("shop")
        .expect("registered player");
    let rendered = attempts[0].embeds[0]
        .fields
        .iter()
        .map(|field| field.value.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        shop.consumables
            .iter()
            .all(|item| { rendered.contains(&item.name) && rendered.contains(&item.description) })
    );
}

fn live_buy_autocomplete_choices(query: &str) -> Vec<CommandOptionChoice> {
    let (database, _provider, _discord) = fixture();
    let connection = Connection::open(database.path()).expect("shop DB");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,pickaxe_tier)
             VALUES (?1,?2,0,275,5,0)",
            params![USER as i64, GUILD as i64],
        )
        .expect("shop tunnel");
    let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database.path())
        .shop(USER as i64, GUILD as i64)
        .expect("shop")
        .expect("registered player");
    super::dig_buy_choices(query, &shop)
}

fn live_buy_autocomplete_values(query: &str) -> BTreeSet<String> {
    live_buy_autocomplete_choices(query)
        .into_iter()
        .map(|choice| match choice {
            CommandOptionChoice::String { name, value } => {
                assert!(!name.trim().is_empty());
                value
            }
            CommandOptionChoice::Integer { .. } | CommandOptionChoice::Number { .. } => {
                panic!("Dig buy choices must be strings")
            }
        })
        .collect()
}

// tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[whetstone-expected0]
#[test]
fn buy_autocomplete_exposes_tempered_whetstone_sink() {
    assert!(live_buy_autocomplete_values("whetstone").contains("tempered_whetstone"));
}

// tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[warding-expected1]
#[test]
fn buy_autocomplete_exposes_warding_salts_sink() {
    assert!(live_buy_autocomplete_values("warding").contains("warding_salts"));
}

// tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[rescue-expected2]
#[test]
fn buy_autocomplete_exposes_rescue_line_sink() {
    assert!(live_buy_autocomplete_values("rescue").contains("rescue_line"));
}

// tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[amulet-expected3]
#[test]
fn buy_autocomplete_exposes_every_high_tier_amulet_sink() {
    let values = live_buy_autocomplete_values("amulet");
    assert!((4..=7).all(|tier| values.contains(&format!("amulet:{tier}"))));
}

// tests/test_dig_buy_command.py::test_buy_autocomplete_exposes_new_sinks[boots-expected4]
#[test]
fn buy_autocomplete_exposes_every_high_tier_boots_sink() {
    let values = live_buy_autocomplete_values("boots");
    assert!((4..=7).all(|tier| values.contains(&format!("boots:{tier}"))));
}

// tests/test_dig_buy_command.py::test_every_shop_row_is_reachable_through_filtered_autocomplete
#[test]
fn live_shop_projection_drives_every_buy_autocomplete_row_with_owned_values() {
    let (database, _provider, _discord) = fixture();
    let connection = Connection::open(database.path()).expect("shop DB");
    connection
        .execute(
            "INSERT INTO tunnels
             (discord_id,guild_id,depth,max_depth,prestige_level,pickaxe_tier)
             VALUES (?1,?2,0,275,5,0)",
            params![USER as i64, GUILD as i64],
        )
        .expect("shop tunnel");
    let shop = cama_app::dig_gear_runtime::DigGearRuntimeService::sqlite(database.path())
        .shop(USER as i64, GUILD as i64)
        .expect("shop")
        .expect("registered player");
    for item in &shop.consumables {
        assert!(super::dig_buy_choices(&item.name.to_ascii_lowercase(), &shop)
            .iter()
            .any(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == &item.id)));
    }
    for item in &shop.pickaxe_upgrades {
        let expected = format!("weapon:{}", item.tier);
        assert!(super::dig_buy_choices(&item.name.to_ascii_lowercase(), &shop)
            .iter()
            .any(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == &expected)));
    }
    for item in &shop.gear_for_sale {
        let expected = format!("{}:{}", item.slot.as_str(), item.tier);
        assert!(super::dig_buy_choices(&item.name.to_ascii_lowercase(), &shop)
            .iter()
            .any(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == &expected)));
    }
}

#[tokio::test]
async fn provider_buy_weapon_uses_sequential_atomic_upgrade_and_equips_it() {
    let (database, provider, _discord) = fixture();
    provider
        .handler
        .handle(go_request(), Arc::new(TestResponder::default()))
        .await
        .expect("first dig");
    let connection = Connection::open(database.path()).expect("upgrade DB");
    connection
        .execute(
            "UPDATE tunnels SET depth=25,max_depth=25
              WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
        )
        .expect("upgrade depth");
    let balance_before: i64 = connection
        .query_row(
            "SELECT jopacoin_balance FROM players
              WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| row.get(0),
        )
        .expect("balance");
    drop(connection);

    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(
                24,
                "buy",
                vec![InteractionOption {
                    name: "item".to_owned(),
                    value: InteractionValue::String("weapon:1".to_owned()),
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("weapon buy");
    assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
    let response = responder.followups.lock().unwrap()[0].clone();
    assert!(response.ephemeral);
    assert_eq!(
        response.content,
        format!("Upgraded your pickaxe to **Stone** for **15** {JOPACOIN_EMOTE}. It is equipped.")
    );
    let connection = Connection::open(database.path()).expect("verify upgrade DB");
    let state = connection
        .query_row(
            "SELECT p.jopacoin_balance,t.pickaxe_tier,
                    (SELECT tier FROM dig_gear
                      WHERE discord_id=p.discord_id AND guild_id=p.guild_id
                        AND slot='weapon' AND equipped=1)
               FROM players p JOIN tunnels t
                 ON t.discord_id=p.discord_id AND t.guild_id=p.guild_id
              WHERE p.discord_id=?1 AND p.guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("upgrade state");
    assert_eq!(state, (balance_before - 15, 1, 1));
}

#[tokio::test]
async fn use_and_gift_autocomplete_are_scoped_to_owned_rows() {
    let (database, provider, _discord) = fixture();
    provider
        .handler
        .handle(go_request(), Arc::new(TestResponder::default()))
        .await
        .expect("first dig");
    let connection = Connection::open(database.path()).expect("owned rows DB");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'dynamite',0,10), (?1,?2,'torch',0,11)",
            params![USER as i64, GUILD as i64],
        )
        .expect("owned items");
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',10,1,0),
                    (?1,?2,'mole_claws',11,1,0),
                    (?1,?2,'ordinary_art',12,0,0)",
            params![USER as i64, GUILD as i64],
        )
        .expect("owned relics");
    drop(connection);

    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            autocomplete_request(25, "use", "item", ""),
            responder.clone(),
        )
        .await
        .expect("use autocomplete");
    provider
        .handler
        .handle(
            autocomplete_request(26, "gift", "artifact", "mole"),
            responder.clone(),
        )
        .await
        .expect("gift autocomplete");
    let choices = responder.autocompletes.lock().unwrap();
    let owned_items = choices[0]
        .iter()
        .filter_map(|choice| match choice {
            CommandOptionChoice::String { value, .. } => Some(value.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(owned_items, BTreeSet::from(["dynamite", "torch"]));
    assert_eq!(
        choices[1]
            .iter()
            .filter(|choice| matches!(choice, CommandOptionChoice::String { value, .. } if value == "mole_claws"))
            .count(),
        2,
        "Python preserves one autocomplete choice per duplicate relic row"
    );
}

#[tokio::test]
async fn command_use_returns_private_embed_with_canonical_item_art() {
    let (database, provider, _discord) = fixture();
    provider
        .handler
        .handle(go_request(), Arc::new(TestResponder::default()))
        .await
        .expect("first dig");
    Connection::open(database.path())
        .expect("use DB")
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'dynamite',0,10)",
            params![USER as i64, GUILD as i64],
        )
        .expect("owned item");
    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(
                27,
                "use",
                vec![InteractionOption {
                    name: "item".to_owned(),
                    value: InteractionValue::String("dynamite".to_owned()),
                }],
            ),
            responder.clone(),
        )
        .await
        .expect("use item");
    assert_eq!(responder.defers.lock().unwrap().as_slice(), &[true]);
    let response = responder.followups.lock().unwrap()[0].clone();
    assert!(response.ephemeral);
    assert_eq!(response.embeds[0].title.as_deref(), Some("Dynamite Queued"));
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some("Ready for your next dig.")
    );
    assert!(
        response.attachments[0]
            .filename
            .starts_with("item_dynamite.")
    );
}

#[tokio::test]
async fn gear_panel_components_use_typed_atomic_service_and_restart_nonce() {
    let (database, provider, _discord) = fixture();
    provider
        .handler
        .handle(go_request(), Arc::new(TestResponder::default()))
        .await
        .expect("first dig");
    let connection = Connection::open(database.path()).expect("gear fixture DB");
    connection
        .execute(
            "UPDATE tunnels SET depth=100,max_depth=100,prestige_level=1
              WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
        )
        .expect("gear gates");
    connection
        .execute(
            "INSERT INTO dig_gear
             (discord_id,guild_id,slot,tier,durability,equipped,acquired_at,source)
             VALUES (?1,?2,'armor',2,5,0,100,'fixture')",
            params![USER as i64, GUILD as i64],
        )
        .expect("armor");
    let armor_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',100,1,0)",
            params![USER as i64, GUILD as i64],
        )
        .expect("relic");
    let relic_id = connection.last_insert_rowid();
    let mut recycle_ids = Vec::new();
    for artifact_id in ["crystal_compass", "obsidian_shield", "mycelium_link"] {
        connection
            .execute(
                "INSERT INTO dig_artifacts
                 (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
                 VALUES (?1,?2,?3,100,1,0)",
                params![USER as i64, GUILD as i64, artifact_id],
            )
            .expect("recyclable relic");
        recycle_ids.push(connection.last_insert_rowid());
    }

    let panel_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(30, "gear", Vec::new()),
            panel_responder.clone(),
        )
        .await
        .expect("gear panel");
    {
        let responses = panel_responder.responses.lock().expect("panel responses");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            responses[0].embeds[0].title.as_deref(),
            Some("Your Loadout")
        );
        assert_eq!(responses[0].components.len(), 1);
        assert_eq!(responses[0].components[0].buttons.len(), 5);
        assert!(responses[0].components[0].buttons.iter().all(|button| {
            button
                .custom_id
                .contains(&provider.handler.state.view_nonce)
        }));
    }

    let open = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                format!(
                    "dig:gear:{}:{}:{}:open:equip:0",
                    provider.handler.state.view_nonce, USER, GUILD
                ),
                Vec::new(),
            ),
            open.clone(),
        )
        .await
        .expect("open selector");
    {
        let updates = open.updates.lock().expect("selector update");
        assert_eq!(updates.len(), 1);
        let update = &updates[0];
        let select = update.components[0]
            .string_select
            .as_ref()
            .expect("gear select");
        assert!(
            select
                .options
                .iter()
                .any(|option| option.value == format!("gear:{armor_id}"))
        );
        assert!(
            select
                .options
                .iter()
                .any(|option| option.value == format!("relic:{relic_id}"))
        );
        assert_eq!(
            update.components[1]
                .buttons
                .iter()
                .map(|button| button.label.as_str())
                .collect::<Vec<_>>(),
            ["Back"]
        );
        let component_ids = update
            .components
            .iter()
            .flat_map(|row| {
                row.buttons
                    .iter()
                    .map(|button| button.custom_id.as_str())
                    .chain(
                        row.string_select
                            .iter()
                            .map(|select| select.custom_id.as_str()),
                    )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            component_ids.iter().copied().collect::<BTreeSet<_>>().len(),
            component_ids.len(),
            "Discord requires every component custom_id to be unique"
        );
    }

    let equip = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                format!(
                    "dig:gear:{}:{}:{}:select:equip:0",
                    provider.handler.state.view_nonce, USER, GUILD
                ),
                vec![format!("gear:{armor_id}")],
            ),
            equip.clone(),
        )
        .await
        .expect("equip");
    assert_eq!(equip.updates.lock().expect("equip update").len(), 1);
    assert!(equip.followups.lock().expect("equip followup")[0].ephemeral);
    assert_eq!(
        Connection::open(database.path())
            .expect("verify DB")
            .query_row(
                "SELECT equipped FROM dig_gear WHERE id=?1",
                [armor_id],
                |row| row.get::<_, i64>(0)
            )
            .expect("equipped"),
        1
    );

    let open_recycle = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                format!(
                    "dig:gear:{}:{}:{}:open:recycle:0",
                    provider.handler.state.view_nonce, USER, GUILD
                ),
                Vec::new(),
            ),
            open_recycle.clone(),
        )
        .await
        .expect("open recycle selector");
    {
        let updates = open_recycle.updates.lock().expect("recycle update");
        assert_eq!(updates.len(), 1);
        let select = updates[0].components[0]
            .string_select
            .as_ref()
            .expect("recycle select");
        assert_eq!(select.min_values, 3);
        assert_eq!(select.max_values, 3);
        assert_eq!(select.options.len(), 3);
        assert!(
            select
                .options
                .iter()
                .all(|option| option.label.starts_with("[Common]"))
        );
    }

    let recycle = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                format!(
                    "dig:gear:{}:{}:{}:recycle",
                    provider.handler.state.view_nonce, USER, GUILD
                ),
                recycle_ids.iter().map(i64::to_string).collect(),
            ),
            recycle.clone(),
        )
        .await
        .expect("recycle relics");
    assert_eq!(recycle.updates.lock().expect("recycle refresh").len(), 1);
    assert!(
        recycle.followups.lock().expect("recycle followup")[0]
            .content
            .contains("Recycled **3 Common** relics")
    );
    let verification = Connection::open(database.path()).expect("recycle verification DB");
    for row_id in recycle_ids {
        assert_eq!(
            verification
                .query_row(
                    "SELECT COUNT(*) FROM dig_artifacts WHERE id=?1",
                    [row_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("recycled source count"),
            0
        );
    }
    assert_eq!(
        verification
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .expect("relic count after recycle"),
        2
    );

    let stale = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(
                format!("dig:gear:old-process:{}:{}:open:equip:0", USER, GUILD),
                Vec::new(),
            ),
            stale.clone(),
        )
        .await
        .expect("stale recovery");
    assert!(
        stale.responses.lock().expect("stale responses")[0]
            .content
            .contains("expired after a restart")
    );
}

#[tokio::test]
async fn provider_help_and_gift_use_typed_social_service_and_python_delivery_contract() {
    const TARGET: u64 = 77_004;
    let (database, provider, _discord) = fixture();
    PlayerRepository::new(database.path())
        .add(&NewPlayer::new(
            TARGET as i64,
            "dig-social-target",
            Some(GUILD as i64),
        ))
        .expect("registered social target");
    let connection = Connection::open(database.path()).expect("social provider fixture DB");
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("match Python legacy foreign-key behavior");
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,boss_progress)
             VALUES (?1,?2,'Target Descent',10,10,0,'{}')",
            params![TARGET as i64, GUILD as i64],
        )
        .expect("target tunnel");

    let user_option = InteractionOption {
        name: "user".to_owned(),
        value: InteractionValue::User {
            id: TARGET,
            display_name: Some("Social Target".to_owned()),
            is_bot: Some(false),
        },
    };
    let help = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(20, "help", vec![user_option.clone()]),
            help.clone(),
        )
        .await
        .expect("typed help provider path");
    assert_eq!(
        help.defers.lock().expect("help defers").as_slice(),
        &[false]
    );
    {
        let help_followups = help.followups.lock().expect("help followups");
        assert_eq!(help_followups.len(), 1);
        assert!(!help_followups[0].ephemeral);
        assert_eq!(
            help_followups[0].embeds[0].title.as_deref(),
            Some("Tunnel Assistance")
        );
        let help_description = help_followups[0].embeds[0]
            .description
            .as_deref()
            .expect("help description");
        assert!(help_description.contains("You helped **dig-social-target**'s tunnel!"));
        assert!(help_description.contains("Blocks added: **"));
    }
    assert_eq!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                 WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3 AND action_type='help'",
                params![GUILD as i64, USER as i64, TARGET as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );

    Connection::open(database.path())
        .unwrap()
        .execute(
            "INSERT INTO dig_artifacts
                (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',100,1,1)",
            params![USER as i64, GUILD as i64],
        )
        .expect("gift relic fixture");
    let gift_options = vec![
        user_option,
        InteractionOption {
            name: "artifact".to_owned(),
            value: InteractionValue::String("mole_claws".to_owned()),
        },
    ];
    let gift = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(21, "gift", gift_options.clone()),
            gift.clone(),
        )
        .await
        .expect("typed gift provider path");
    assert_eq!(
        gift.defers.lock().expect("gift defers").as_slice(),
        &[false]
    );
    {
        let gift_followups = gift.followups.lock().expect("gift followups");
        assert_eq!(gift_followups.len(), 1);
        assert!(!gift_followups[0].ephemeral);
        assert_eq!(
            gift_followups[0].content,
            "You gifted **Mole Claws** to **dig-social-target**!"
        );
    }
    assert_eq!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM dig_artifacts
                 WHERE discord_id=?1 AND guild_id=?2 AND artifact_id='mole_claws'",
                params![TARGET as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );

    let failed = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(command_request(22, "gift", gift_options), failed.clone())
        .await
        .expect("failed gift is a typed user response");
    let failed_followups = failed.followups.lock().expect("failed gift followups");
    assert_eq!(failed_followups.len(), 1);
    assert!(failed_followups[0].ephemeral);
    assert_eq!(failed_followups[0].content, "You don't have that artifact.");
}

#[tokio::test]
async fn provider_sabotage_view_is_owner_bound_restart_expiring_and_exactly_once() {
    const TARGET: u64 = 77_004;
    const COPIED_ACTOR: u64 = 77_005;
    let (database, provider, _discord) = fixture();
    let players = PlayerRepository::new(database.path());
    players
        .add(&NewPlayer::new(
            TARGET as i64,
            "dig-sabotage-target",
            Some(GUILD as i64),
        ))
        .expect("registered sabotage target");
    players
        .add(&NewPlayer::new(
            COPIED_ACTOR as i64,
            "dig-sabotage-copy",
            Some(GUILD as i64),
        ))
        .expect("registered copied-view actor");
    let connection = Connection::open(database.path()).expect("sabotage provider fixture DB");
    connection
        .execute(
            "UPDATE players SET jopacoin_balance=0
             WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET as i64, GUILD as i64],
        )
        .expect("zero target balance for conservation assertion");
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,boss_progress)
             VALUES (?1,?2,'Attacker Descent',20,20,0,'{}')",
            params![USER as i64, GUILD as i64],
        )
        .expect("attacker tunnel");
    connection
        .execute(
            "INSERT INTO tunnels
                (discord_id,guild_id,tunnel_name,depth,max_depth,last_dig_at,
                 boss_progress,trap_active)
             VALUES (?1,?2,'Target Descent',100,100,0,'{}',1)",
            params![TARGET as i64, GUILD as i64],
        )
        .expect("trapped target tunnel");

    let preview_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            command_request(
                23,
                "sabotage",
                vec![InteractionOption {
                    name: "user".to_owned(),
                    value: InteractionValue::User {
                        id: TARGET,
                        display_name: Some("Sabotage Target".to_owned()),
                        is_bot: Some(false),
                    },
                }],
            ),
            preview_responder.clone(),
        )
        .await
        .expect("typed sabotage preview");
    let preview = preview_responder.responses.lock().unwrap()[0].clone();
    assert!(!preview.ephemeral);
    assert_eq!(preview.embeds[0].title.as_deref(), Some("Confirm Sabotage"));
    assert_eq!(
        preview.embeds[0]
            .description
            .as_deref()
            .expect("sabotage preview description"),
        format!(
            "**Target:** dig-sabotage-target\n**Cost:** 20 {JOPACOIN_EMOTE}\n**Potential damage:** 3-8 blocks\n\nAre you sure? If they have a trap set, you could take damage instead."
        )
    );
    assert_eq!(preview.components[0].buttons.len(), 2);
    let confirm_id = preview.components[0].buttons[0].custom_id.clone();
    assert!(confirm_id.starts_with("dig:sabotage:confirm:"));

    let copied = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request_as(COPIED_ACTOR, "Copied Actor", &confirm_id, Vec::new()),
            copied.clone(),
        )
        .await
        .expect("copied sabotage view rejected");
    assert_eq!(
        copied.responses.lock().unwrap()[0].content,
        "This isn't your sabotage."
    );

    let restarted = DigRegistrationProvider::with_media(
        database.path(),
        &config(),
        Arc::new(TestDiscord::default()),
        None,
        Arc::clone(&provider.handler.state.media),
    );
    let expired = Arc::new(TestResponder::default());
    restarted
        .handler
        .handle(component_request(&confirm_id, Vec::new()), expired.clone())
        .await
        .expect("restart expires process-local sabotage view");
    assert_eq!(
        expired.responses.lock().unwrap()[0].content,
        "This sabotage expired. Use `/dig sabotage` again."
    );

    let committed = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(&confirm_id, Vec::new()),
            committed.clone(),
        )
        .await
        .expect("trapped sabotage settlement");
    let update = committed.updates.lock().unwrap()[0].clone();
    assert_eq!(update.embeds[0].title.as_deref(), Some("Trap Triggered!"));
    assert!(
        update.embeds[0]
            .description
            .as_deref()
            .is_some_and(|description| {
                description.starts_with("Your sabotage attempt backfired!\nTrap triggered!")
                    && description.contains("You lost 40 JC")
                    && description.contains("blocks!")
            })
    );
    assert!(update.components.is_empty());

    let state = Connection::open(database.path()).expect("verify sabotage settlement");
    assert_eq!(
        state
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        460
    );
    let target_balance = state
        .query_row(
            "SELECT jopacoin_balance FROM players
             WHERE discord_id=?1 AND guild_id=?2",
            params![TARGET as i64, GUILD as i64],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let sabotage_detail: String = state
        .query_row(
            "SELECT detail FROM dig_actions
             WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3
               AND action_type='sabotage'",
            params![GUILD as i64, USER as i64, TARGET as i64],
            |row| row.get(0),
        )
        .unwrap();
    let victim_tip = serde_json::from_str::<serde_json::Value>(&sabotage_detail)
        .expect("typed sabotage audit detail")["victim_tip"]
        .as_i64()
        .expect("victim tip audit value");
    assert_eq!(target_balance, 20 + victim_tip);
    let (actor_depth, trap_active, action_count): (i64, i64, i64) = (
        state
            .query_row(
                "SELECT depth FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get(0),
            )
            .unwrap(),
        state
            .query_row(
                "SELECT trap_active FROM tunnels WHERE discord_id=?1 AND guild_id=?2",
                params![TARGET as i64, GUILD as i64],
                |row| row.get(0),
            )
            .unwrap(),
        state
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                 WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3
                   AND action_type='sabotage'",
                params![GUILD as i64, USER as i64, TARGET as i64],
                |row| row.get(0),
            )
            .unwrap(),
    );
    assert!((15..=17).contains(&actor_depth));
    assert_eq!(trap_active, 0);
    assert_eq!(action_count, 1);

    let duplicate = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            component_request(&confirm_id, Vec::new()),
            duplicate.clone(),
        )
        .await
        .expect("duplicate sabotage component is admitted as a user error");
    assert_eq!(
        duplicate.responses.lock().unwrap()[0].content,
        "This sabotage was already resolved."
    );
    assert_eq!(
        state
            .query_row(
                "SELECT COUNT(*) FROM dig_actions
                 WHERE guild_id=?1 AND actor_id=?2 AND target_id=?3
                   AND action_type='sabotage'",
                params![GUILD as i64, USER as i64, TARGET as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn sabotage_result_embed_keeps_prediction_contract_detail() {
    let response = super::sabotage_result_response(
        &cama_app::dig_social_runtime::DigSabotageResult {
            cost: 20,
            damage: 7,
            target_tunnel: "Target Descent".to_owned(),
            target_depth_after: 93,
            trap_triggered: false,
            trap_detail: None,
            sabotage_hit: true,
            clue: None,
            is_reveal: false,
            insurance_applied: false,
            damage_reduced: false,
            absorbed_by_aegis: false,
            protection_source: None,
            victim_tip: 0,
            mana_steal_jc: 0,
            attacker_block_reward: 5,
            vendetta_reflect: 0,
            vendetta_bonus: 0,
            prediction_contract_steal: Some(
                cama_app::dig_social_runtime::DigPredictionContractSteal {
                    prediction_id: 42,
                    side: "yes",
                    contracts: 3,
                },
            ),
            action_id: 9,
        },
        "Sabotage Target",
    );
    assert_eq!(
        response.embeds[0].description.as_deref(),
        Some(
            "You sabotaged **Sabotage Target**'s tunnel!\nDamage dealt: **7** blocks\nStole **3 YES** prediction contracts from market **#42**."
        )
    );
}

#[tokio::test]
async fn provider_miner_group_uses_typed_profile_allocation_respec_and_autobuy_service() {
    let (database, provider, _discord) = fixture();

    let profile_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            grouped_command_request(24, "miner", "profile", Vec::new()),
            profile_responder.clone(),
        )
        .await
        .expect("typed miner profile");
    assert_eq!(profile_responder.defers.lock().unwrap().as_slice(), &[true]);
    let profile = profile_responder.followups.lock().unwrap()[0].clone();
    assert!(profile.ephemeral);
    assert_eq!(
        profile.embeds[0].title.as_deref(),
        Some("dig-test-miner - Miner Profile")
    );
    assert_eq!(
        profile.embeds[0].description.as_deref(),
        Some("Backstory not set.")
    );
    assert_eq!(
        profile.embeds[0].fields[0].value,
        "Strength **0** | Smarts **0** | Stamina **0**\nPoints: **5** total, **5** unspent\nEffects: +0/+0 advance range, -0% cave-in, -0% cooldown/paid costs"
    );
    assert_eq!(
        profile.embeds[0].footer.as_deref(),
        Some("Backstory locks after you set it. Boss first clears grant one extra S point.")
    );

    let about_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            grouped_command_request(
                25,
                "miner",
                "about",
                vec![InteractionOption {
                    name: "backstory".to_owned(),
                    value: InteractionValue::String(" Former  cartographer @everyone ".to_owned()),
                }],
            ),
            about_responder.clone(),
        )
        .await
        .expect("typed miner backstory");
    let about = about_responder.responses.lock().unwrap()[0].clone();
    assert!(about.ephemeral);
    assert_eq!(
        about.embeds[0].title.as_deref(),
        Some("Backstory Locked In")
    );
    assert_eq!(
        about.embeds[0].description.as_deref(),
        Some("Former cartographer (at)everyone")
    );
    assert_eq!(
        about.embeds[0].footer.as_deref(),
        Some("This cannot be changed later.")
    );

    let build_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            grouped_command_request(
                26,
                "miner",
                "build",
                vec![
                    InteractionOption {
                        name: "strength".to_owned(),
                        value: InteractionValue::Integer(2),
                    },
                    InteractionOption {
                        name: "smarts".to_owned(),
                        value: InteractionValue::Integer(2),
                    },
                    InteractionOption {
                        name: "stamina".to_owned(),
                        value: InteractionValue::Integer(1),
                    },
                ],
            ),
            build_responder.clone(),
        )
        .await
        .expect("typed miner allocation");
    assert_eq!(build_responder.defers.lock().unwrap().as_slice(), &[true]);
    let build = build_responder.followups.lock().unwrap()[0].clone();
    assert_eq!(build.embeds[0].title.as_deref(), Some("S Points Spent"));
    assert_eq!(
        build.embeds[0].description.as_deref(),
        Some(
            "Strength **2** | Smarts **2** | Stamina **1**\nPoints: **5** total, **0** unspent\nEffects: +0/+1 advance range, -4% cave-in, -4% cooldown/paid costs"
        )
    );

    let respec_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            grouped_command_request(27, "miner", "respec", Vec::new()),
            respec_responder.clone(),
        )
        .await
        .expect("typed miner respec");
    let respec = respec_responder.followups.lock().unwrap()[0].clone();
    assert_eq!(respec.embeds[0].title.as_deref(), Some("S Points Reset"));
    assert_eq!(
        respec.embeds[0].description.as_deref(),
        Some("Returned **5** allocated S points. You now have **5** unspent S points.")
    );
    assert_eq!(
        respec.embeds[0].footer.as_deref(),
        Some("50 JC spent on the respec.")
    );
    assert_eq!(
        Connection::open(database.path())
            .unwrap()
            .query_row(
                "SELECT jopacoin_balance FROM players
                 WHERE discord_id=?1 AND guild_id=?2",
                params![USER as i64, GUILD as i64],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        450
    );

    let autobuy_responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            grouped_command_request(
                28,
                "miner",
                "autobuy",
                vec![
                    InteractionOption {
                        name: "item".to_owned(),
                        value: InteractionValue::String("both".to_owned()),
                    },
                    InteractionOption {
                        name: "enabled".to_owned(),
                        value: InteractionValue::Boolean(true),
                    },
                ],
            ),
            autobuy_responder.clone(),
        )
        .await
        .expect("typed miner auto-buy");
    let autobuy = autobuy_responder.followups.lock().unwrap()[0].clone();
    assert_eq!(
        autobuy.embeds[0].title.as_deref(),
        Some("Dig Auto-Buy Updated")
    );
    assert_eq!(
        autobuy.embeds[0].description.as_deref(),
        Some("Torch: **ON**\nHard Hat: **ON**")
    );
    assert_eq!(
        autobuy.embeds[0].footer.as_deref(),
        Some("Auto-buy spends JC only when an actual dig starts.")
    );
    let settings: (i64, i64) = Connection::open(database.path())
        .unwrap()
        .query_row(
            "SELECT auto_buy_torch,auto_buy_hard_hat FROM tunnels
             WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(settings, (1, 1));
}

#[tokio::test]
async fn provider_registration_channel_gate_and_autocomplete_are_live() {
    let (database, provider, _discord) = fixture();
    let connection = Connection::open(database.path()).expect("autocomplete fixture DB");
    connection
        .execute(
            "INSERT INTO tunnels(discord_id,guild_id,depth,max_depth,pickaxe_tier)
             VALUES (?1,?2,0,0,0)",
            params![USER as i64, GUILD as i64],
        )
        .expect("autocomplete tunnel");
    connection
        .execute(
            "INSERT INTO dig_inventory
             (discord_id,guild_id,item_type,queued,created_at)
             VALUES (?1,?2,'hard_hat',0,1)",
            params![USER as i64, GUILD as i64],
        )
        .expect("owned autocomplete item");
    connection
        .execute(
            "INSERT INTO dig_artifacts
             (discord_id,guild_id,artifact_id,found_at,is_relic,equipped)
             VALUES (?1,?2,'mole_claws',1,1,0)",
            params![USER as i64, GUILD as i64],
        )
        .expect("owned autocomplete relic");
    let mut registry = crate::registration::RegistryBuilder::default();
    provider
        .register(&mut registry)
        .expect("Dig command and component route register");
    let registry = registry.build();
    assert_eq!(registry.commands().count(), 1);
    assert_eq!(
        registry
            .component_routes()
            .iter()
            .map(|route| route.custom_id_prefix.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["dig:", "dig_route_", "duel_opt_"])
    );

    let responder = Arc::new(TestResponder::default());
    provider
        .handler
        .handle(
            autocomplete_request(5, "use", "item", "hard"),
            responder.clone(),
        )
        .await
        .expect("item autocomplete should respond");
    provider
        .handler
        .handle(
            autocomplete_request(6, "buy", "item", "stone"),
            responder.clone(),
        )
        .await
        .expect("buy autocomplete should respond");
    provider
        .handler
        .handle(
            autocomplete_request(7, "gift", "artifact", "mole"),
            responder.clone(),
        )
        .await
        .expect("artifact autocomplete should respond");
    {
        let choices = responder
            .autocompletes
            .lock()
            .expect("autocomplete choices");
        assert_eq!(choices.len(), 3);
        assert!(choices[0].iter().any(|choice| matches!(
            choice,
            CommandOptionChoice::String { value, .. } if value == "hard_hat"
        )));
        assert!(choices[1].iter().any(|choice| matches!(
            choice,
            CommandOptionChoice::String { value, .. } if value == "weapon:1"
        )));
        assert!(choices[2].iter().any(|choice| matches!(
            choice,
            CommandOptionChoice::String { value, .. } if value == "mole_claws"
        )));
    }

    let balance_before: i64 = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| row.get(0),
        )
        .expect("balance before gate");
    // The transport adapter's false result must debit exactly one JC
    // through the app service before it rejects the command.
    let gated_discord = Arc::new(TestDiscord {
        gamba: false,
        avatar_url: None,
        ..TestDiscord::default()
    });
    let gated_provider =
        DigRegistrationProvider::new(database.path(), &config(), gated_discord, None);
    let gate_responder = Arc::new(TestResponder::default());
    gated_provider
        .handler
        .handle(
            command_request(6, "guide", Vec::new()),
            gate_responder.clone(),
        )
        .await
        .expect("wrong-channel admission should be handled");
    let gate_response = gate_responder.responses.lock().expect("gate response");
    assert_eq!(gate_response.len(), 1);
    assert!(gate_response[0].content.contains("not consecrated"));
    drop(gate_response);
    let balance_after: i64 = Connection::open(database.path())
        .expect("reopen database")
        .query_row(
            "SELECT jopacoin_balance FROM players WHERE discord_id=?1 AND guild_id=?2",
            params![USER as i64, GUILD as i64],
            |row| row.get(0),
        )
        .expect("balance after gate");
    assert_eq!(balance_after, balance_before - 1);
}

fn find<'a>(options: &'a [CommandOptionSpec], name: &str) -> &'a CommandOptionSpec {
    options
        .iter()
        .find(|option| option.name == name)
        .unwrap_or_else(|| panic!("missing Dig option {name}"))
}

#[test]
fn command_tree_matches_python_surface() {
    let options = dig_options();
    assert_eq!(options.len(), 22);
    for (name, description) in [
        ("go", "Dig deeper into your tunnel"),
        ("help", "Help another player's tunnel"),
        ("sabotage", "Sabotage another player's tunnel"),
        ("info", "View tunnel information"),
        ("leaderboard", "View top tunnels"),
        (
            "halloffame",
            "View the hall of fame (best prestige run scores)",
        ),
        ("use", "Queue a consumable for your next dig"),
        ("gift", "Gift a relic to another player"),
        ("shop", "Browse the mining shop"),
        ("buy", "Buy an item from the mining shop"),
        ("flex", "Show off your mining stats"),
        (
            "prestige",
            "Prestige your tunnel (reset depth, gain a perk)",
        ),
        ("abandon", "Abandon your tunnel (partial refund)"),
        ("trap", "Set a trap in your tunnel"),
        ("insure", "Buy cave-in insurance"),
        ("inventory", "View your mining inventory"),
        ("artifacts", "View all artifacts and the ones you own"),
        ("gear", "Manage your boss-combat gear"),
        ("weather", "View today's layer weather conditions"),
        ("guide", "Learn how to dig"),
    ] {
        assert_eq!(find(&options, name).description, description);
    }

    let admin = find(&options, "admin");
    assert_eq!(admin.kind, CommandOptionKind::SubcommandGroup);
    for (name, description, user_description) in [
        (
            "resetcooldown",
            "Reset a player's free dig cooldown (Admin only)",
            "The player whose cooldown to reset",
        ),
        (
            "forceevent",
            "Force next dig to trigger an event (Admin only)",
            "The player whose next dig gets an event",
        ),
        (
            "setdepth",
            "Set a player's tunnel depth (Admin only)",
            "The player",
        ),
    ] {
        let command = find(&admin.options, name);
        assert_eq!(command.description, description);
        let user = find(&command.options, "user");
        assert_eq!(user.kind, CommandOptionKind::User);
        assert!(user.required);
        assert_eq!(user.description, user_description);
    }
    let setdepth = find(&admin.options, "setdepth");
    assert_eq!(
        find(&setdepth.options, "depth").kind,
        CommandOptionKind::Integer
    );

    let miner = find(&options, "miner");
    assert_eq!(miner.kind, CommandOptionKind::SubcommandGroup);
    for (name, description) in [
        ("profile", "View your miner profile and S stats"),
        ("about", "Set your miner backstory once"),
        (
            "build",
            "Spend unallocated points on Strength, Smarts, and Stamina",
        ),
        ("respec", "Reset your allocated S points for 50 JC"),
        ("autobuy", "Auto-buy Torch and/or Hard Hat for each dig"),
    ] {
        assert_eq!(find(&miner.options, name).description, description);
    }
    let about = find(&miner.options, "about");
    let backstory = find(&about.options, "backstory");
    assert_eq!(
        backstory.description,
        "Short backstory blurb for the AI Dungeon Master"
    );
    assert_eq!(backstory.max_length, Some(500));

    let build = find(&miner.options, "build");
    for (name, description) in [
        (
            "strength",
            "Points to add. Increases how far you dig each action.",
        ),
        (
            "smarts",
            "Points to add. Helps you read the stone and avoid collapses.",
        ),
        (
            "stamina",
            "Points to add. Keeps you digging longer between rests.",
        ),
    ] {
        let stat = find(&build.options, name);
        assert_eq!(stat.kind, CommandOptionKind::Integer);
        assert_eq!(stat.description, description);
    }
    let autobuy = find(&miner.options, "autobuy");
    let item = find(&autobuy.options, "item");
    assert_eq!(item.choices.len(), 3);
    assert_eq!(
        item.choices,
        vec![
            CommandOptionChoice::String {
                name: "Torch".to_owned(),
                value: "torch".to_owned(),
            },
            CommandOptionChoice::String {
                name: "Hard Hat".to_owned(),
                value: "hard_hat".to_owned(),
            },
            CommandOptionChoice::String {
                name: "Both".to_owned(),
                value: "both".to_owned(),
            },
        ]
    );
    assert_eq!(
        find(&autobuy.options, "enabled").description,
        "Whether to auto-buy this item on each real dig"
    );

    for (name, option_name) in [("help", "user"), ("sabotage", "user"), ("gift", "user")] {
        assert_eq!(
            find(&find(&options, name).options, option_name).kind,
            CommandOptionKind::User
        );
    }
    assert!(find(&find(&options, "use").options, "item").autocomplete);
    assert!(find(&find(&options, "gift").options, "artifact").autocomplete);
    assert!(find(&find(&options, "buy").options, "item").autocomplete);
}
