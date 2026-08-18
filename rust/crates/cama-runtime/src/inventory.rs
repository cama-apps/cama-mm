//! Mechanical production cutover inventory extracted from `bot.py`.
//!
//! An item is `Wired` only when the production Discord runtime registers or
//! supervises the real Rust adapter. A passing domain parity test does not
//! change runtime wiring status. This makes every missing live command,
//! component, event, worker, and provider explicit rather than an acceptable
//! gap hidden behind a successful gateway connection.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CutoverStatus {
    Wired,
    Required,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CutoverItem {
    pub python_name: &'static str,
    pub rust_boundary: &'static str,
    pub status: CutoverStatus,
}

pub const PYTHON_EXTENSIONS: &[CutoverItem] = &[
    wired(
        "commands.registration",
        "PlayerRegistrationProvider + persistent modal/DM/lobby-observer composition",
    ),
    wired(
        "commands.info",
        "InfoRegistrationProvider + configured SQLite analytics + native statistical media",
    ),
    wired(
        "commands.lobby",
        "LobbyRegistrationProvider + durable SQLite moderation/audit + ready/Neon/registration-observer composition",
    ),
    wired(
        "commands.match",
        "MatchRegistrationProvider + shared live lobby/enrichment/Discord runtime + durable pending-match READY recovery",
    ),
    wired(
        "commands.admin",
        "AdminRegistrationProvider + shared live Discord/lobby/match/OpenDota/UsageMonitor controls + durable correction reward/bet/OpenSkill READY recovery",
    ),
    wired(
        "commands.betting",
        "BettingRegistrationProvider + shared vanity-tax/wager-refresh/debrief/flavor composition + supervised live-view timeout",
    ),
    wired(
        "commands.advstats",
        "AdvancedStatsRegistrationProvider + migrated SQLite player/pairing reads",
    ),
    wired(
        "commands.enrichment",
        "EnrichmentRegistrationProvider + atomic OpenDota/Scout/Wrapped/OpenSkill projections + restart replay",
    ),
    wired(
        "commands.dota_info",
        "DotaInfoRegistrationProvider + bundled read-only Dotabase catalog + autocomplete",
    ),
    wired(
        "commands.shop",
        "ShopRegistrationProvider + migrated SQLite atomic purchases/protection + live Serenity media/temporary-message delivery",
    ),
    wired(
        "commands.predictions",
        "PredictionRegistrationProvider + live Serenity threads/components + SQLite settlement + native Neon media",
    ),
    wired(
        "commands.duel",
        "DuelRegistrationProvider + persistent controls/restart recovery + supervised lifecycle worker",
    ),
    wired(
        "commands.tax",
        "TaxRegistrationProvider + migrated SQLite economy/vanity enforcement + scoped ephemeral pagers",
    ),
    wired(
        "commands.blame_luke",
        "BlameLukeRegistrationProvider + atomic SQLite wallet/refund + native persistent GIF interaction",
    ),
    wired(
        "commands.ask",
        "configured AskRegistrationProvider + guild-scoped read-only SQLite/LLM adapter",
    ),
    wired(
        "commands.profile",
        "ProfileRegistrationProvider + live SQLite/OpenDota tabs",
    ),
    wired(
        "commands.draft",
        "DraftRegistrationProvider + shared lobby/draft state + Match reminder/Neon adapter composition",
    ),
    wired(
        "commands.rating_analysis",
        "RatingAnalysisRegistrationProvider + configured durable replay + native statistical media",
    ),
    wired("commands.herogrid", "HeroGridRegistrationProvider"),
    wired(
        "commands.scout",
        "ScoutRegistrationProvider + shared live match/draft/lobby context + native paginated portrait media",
    ),
    wired(
        "commands.wrapped",
        "WrappedRegistrationProvider + complete SQLite story projections + native 16-page media lifecycle",
    ),
    wired(
        "commands.trivia",
        "TriviaRegistrationProvider + pinned Dotabase catalog + live media/payout/component lifecycle",
    ),
    wired(
        "commands.player_trivia",
        "PlayerTriviaRegistrationProvider + durable frozen SQLite sessions + exhaustive typed guild snapshots",
    ),
    wired(
        "commands.mana",
        "ManaRegistrationProvider + migrated SQLite assignment/protection + live Serenity member/role paging",
    ),
    wired(
        "commands.dig",
        "DigRegistrationProvider + durable SQLite mechanics/event outboxes + bonus components + READY reconciliation + native media/flavor hooks",
    ),
    wired(
        "commands.mafia",
        "MafiaRegistrationProvider + durable SQLite phase/setup/settlement recovery + private-thread Discord composition",
    ),
    wired(
        "commands.pet (PET_CHANNEL_ID conditional)",
        "PetRegistrationProvider + conditional SQLite commands/components + brawl/eating/media/restart lifecycle",
    ),
    wired(
        "commands.reminders",
        "ReminderRegistrationProvider + five persistent buttons + SQLite preferences",
    ),
    wired(
        "commands.survey",
        "SurveyRegistrationProvider + nonce-safe DM/components + READY reconciliation + sliding result pager",
    ),
];

pub const PYTHON_GATEWAY_EVENTS: &[CutoverItem] = &[
    wired("setup_hook", "RegistryBuilder providers before connect"),
    wired(
        "on_ready",
        "Serenity ready lifecycle; global replacement remains cutover-guarded",
    ),
    wired("gateway resume", "Serenity resume lifecycle event"),
    wired(
        "on_member_join",
        "VanityTaxGatewayObserver + Serenity server-nickname adapter",
    ),
    wired(
        "on_member_update",
        "VanityTaxGatewayObserver + Serenity raw member-update adapter",
    ),
    wired(
        "on_member_remove",
        "VanityTaxGatewayObserver + Serenity member-removal adapter",
    ),
    wired(
        "bot.tree.error",
        "GlobalInteractionHooks typed policy + Serenity command dispatch",
    ),
    wired(
        "on_interaction",
        "UsageMonitor observer in live Serenity command dispatch",
    ),
    wired(
        "on_raw_reaction_add",
        "Serenity raw add dispatch + LobbyRawReactionObserver suspension/ready/Neon auxiliary composition",
    ),
    wired(
        "on_raw_reaction_remove",
        "Serenity raw remove dispatch + LobbyRawReactionObserver",
    ),
];

pub const PYTHON_READY_RECOVERY_ACTIONS: &[CutoverItem] = &[
    wired(
        "refresh vanity-tax membership snapshots",
        "paginated Serenity HTTP ready/resume recovery observer",
    ),
    wired(
        "reconcile persisted lobby messages",
        "LobbyGatewayObserver on Serenity ready/resume",
    ),
    wired(
        "warm trivia image cache",
        "TriviaImageCacheWarmObserver on Serenity ready/resume",
    ),
    wired(
        "backfill inferred player regions",
        "PlayerRegistrationProvider READY observer + deduplicated OpenDota counts/bulk SQLite update",
    ),
    wired(
        "reschedule stored reminders",
        "ReminderRecoveryObserver + authoritative wheel/trivia/dig/pet anchors",
    ),
];

pub const PYTHON_BACKGROUND_TASKS: &[CutoverItem] = &[
    wired(
        "prediction_refresh",
        "PredictionRefreshWorker + transactional publication outbox and live message refresh",
    ),
    wired(
        "prediction_digest",
        "PredictionDigestWorker + durable digest cursor/outbox and PNG delivery",
    ),
    wired(
        "manashop_debt",
        "ManashopDebtWorker + atomic due Dark Bargain settlement",
    ),
    wired(
        "duel_challenges",
        "DuelChallengesWorker + durable claim/rearm/expiry delivery",
    ),
    wired(
        "economy_events (ECONOMY_EVENTS_ENABLED conditional)",
        "EconomyEventsWorker + atomic activation and durable announcement stamps",
    ),
    wired(
        "first_game_pool (amount conditional)",
        "FirstGamePoolWorker + atomic funding/display-refresh outbox",
    ),
    wired(
        "dig weather broadcast",
        "DigWeatherWorker + atomic two-layer daily forecast + live Discord rollover delivery",
    ),
    wired(
        "mafia phase loop",
        "MafiaPhaseWorker + five-minute guild sweep + durable reminder/phase/outbox recovery",
    ),
    wired(
        "pet sweep (PET_CHANNEL_ID conditional)",
        "PetSweepWorker + stale-brawl-first durable lifecycle delivery + exact AI/fallback flavor + authored/native media + reminder/DM retry policy",
    ),
    wired(
        "survey scheduled-delivery recovery",
        "SurveyRecoveryWorker + bounded delivery/reconciliation retries",
    ),
];

pub const EXTERNAL_PROVIDERS: &[CutoverItem] = &[
    wired(
        "OpenDota HTTP",
        "shared reqwest adapter consumed by /profile Dota component",
    ),
    wired(
        "OpenAI-compatible LLM HTTP",
        "OpenAiCompatibleProvider + SQLite attempt telemetry",
    ),
    wired(
        "Discord PNG byte attachment upload (Hero Grid)",
        "typed response + Serenity attachment adapter",
    ),
    wired(
        "remaining Discord media upload/edit surfaces",
        "typed command/worker upload, replacement, preservation, clear, and delete lifecycles + Serenity transport",
    ),
    wired(
        "image/chart rendering",
        "production native PNG/GIF/chart renderers + typed attachment delivery/edit/retry adapters",
    ),
];

pub const CONFIGURATION_DOMAINS: &[CutoverItem] = &[
    wired("DISCORD_BOT_TOKEN", "RuntimeConfig secret"),
    wired("DB_PATH", "RuntimeConfig shared SQLite path"),
    wired(
        "Discord channel IDs",
        "ApplicationConfig::channels typed production snapshot",
    ),
    wired(
        "administrator/tax-user IDs",
        "ApplicationConfig::identities typed production snapshot",
    ),
    wired(
        "lobby/shuffle/rating values",
        "ApplicationConfig::values + typed migration/service consumption",
    ),
    wired(
        "economy/betting/prediction values",
        "ApplicationConfig::values production-consumable graph",
    ),
    wired(
        "dig/mafia/pet values",
        "typed values/channels with service feature gates",
    ),
    wired(
        "LLM key/model/timeouts",
        "redacted provider-bound ApplicationConfig snapshot",
    ),
];

#[must_use]
pub fn required_count() -> usize {
    all_items()
        .filter(|item| item.status == CutoverStatus::Required)
        .count()
}

/// Return whether the runtime may replace Discord's existing global command
/// tree. A non-empty required inventory keeps synchronization disabled.
#[must_use]
pub const fn global_command_sync_allowed(required: usize) -> bool {
    required == 0
}

#[must_use]
pub fn wired_count() -> usize {
    all_items()
        .filter(|item| item.status == CutoverStatus::Wired)
        .count()
}

fn all_items() -> impl Iterator<Item = &'static CutoverItem> {
    PYTHON_EXTENSIONS
        .iter()
        .chain(PYTHON_GATEWAY_EVENTS)
        .chain(PYTHON_READY_RECOVERY_ACTIONS)
        .chain(PYTHON_BACKGROUND_TASKS)
        .chain(EXTERNAL_PROVIDERS)
        .chain(CONFIGURATION_DOMAINS)
}

const fn wired(python_name: &'static str, rust_boundary: &'static str) -> CutoverItem {
    CutoverItem {
        python_name,
        rust_boundary,
        status: CutoverStatus::Wired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_SOURCE: &str = include_str!("main.rs");
    const SERENITY_TRANSPORT_SOURCE: &str = include_str!("serenity_transport.rs");

    #[test]
    fn all_python_extensions_are_explicit_without_overclaiming_partial_lobby_wiring() {
        assert_eq!(PYTHON_EXTENSIONS.len(), 29);
        assert_eq!(
            PYTHON_EXTENSIONS
                .iter()
                .filter(|item| item.status == CutoverStatus::Wired)
                .map(|item| item.python_name)
                .collect::<Vec<_>>(),
            [
                "commands.registration",
                "commands.info",
                "commands.lobby",
                "commands.match",
                "commands.admin",
                "commands.betting",
                "commands.advstats",
                "commands.enrichment",
                "commands.dota_info",
                "commands.shop",
                "commands.predictions",
                "commands.duel",
                "commands.tax",
                "commands.blame_luke",
                "commands.ask",
                "commands.profile",
                "commands.draft",
                "commands.rating_analysis",
                "commands.herogrid",
                "commands.scout",
                "commands.wrapped",
                "commands.trivia",
                "commands.player_trivia",
                "commands.mana",
                "commands.dig",
                "commands.mafia",
                "commands.pet (PET_CHANNEL_ID conditional)",
                "commands.reminders",
                "commands.survey",
            ]
        );
    }

    #[test]
    fn live_profile_marks_concrete_opendota_transport_wired() {
        assert_eq!(
            EXTERNAL_PROVIDERS[0],
            wired(
                "OpenDota HTTP",
                "shared reqwest adapter consumed by /profile Dota component",
            )
        );
    }

    #[test]
    fn remaining_discord_media_lifecycles_and_followup_preservation_are_wired() {
        assert_eq!(
            EXTERNAL_PROVIDERS[3],
            wired(
                "remaining Discord media upload/edit surfaces",
                "typed command/worker upload, replacement, preservation, clear, and delete lifecycles + Serenity transport",
            )
        );
        for seam in [
            "struct ComponentOnlyInteractionEdit",
            "fn component_only_interaction_edit(",
            "async fn edit_component_only_followup(",
            "http.edit_followup_message(interaction_token, message_id, &edit, Vec::new())",
            "auto_component_only_followup_uses_attachment_omitting_raw_payload",
            "component_only_explicit_clear_serializes_empty_attachments",
            "component_only_followup_transport_preserves_existing_media",
        ] {
            assert!(
                SERENITY_TRANSPORT_SOURCE.contains(seam),
                "missing attachment-preserving followup seam {seam}"
            );
        }
    }

    #[test]
    fn live_enrichment_commands_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[7],
            wired(
                "commands.enrichment",
                "EnrichmentRegistrationProvider + atomic OpenDota/Scout/Wrapped/OpenSkill projections + restart replay",
            )
        );
    }

    #[test]
    fn live_trivia_command_and_cache_recovery_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[21],
            wired(
                "commands.trivia",
                "TriviaRegistrationProvider + pinned Dotabase catalog + live media/payout/component lifecycle",
            )
        );
        assert_eq!(
            PYTHON_READY_RECOVERY_ACTIONS[2],
            wired(
                "warm trivia image cache",
                "TriviaImageCacheWarmObserver on Serenity ready/resume",
            )
        );
    }

    #[test]
    fn live_mana_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[23],
            wired(
                "commands.mana",
                "ManaRegistrationProvider + migrated SQLite assignment/protection + live Serenity member/role paging",
            )
        );
    }

    #[test]
    fn live_player_trivia_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[22],
            wired(
                "commands.player_trivia",
                "PlayerTriviaRegistrationProvider + durable frozen SQLite sessions + exhaustive typed guild snapshots",
            )
        );
    }

    #[test]
    fn newly_promoted_live_providers_are_retained_by_production_main() {
        for seam in [
            "InfoRegistrationProvider::new(",
            "registry.add_provider(&info_provider)",
            "RatingAnalysisRegistrationProvider::new(",
            "registry.add_provider(&rating_analysis_provider)",
            "BlameLukeRegistrationProvider::new(",
            "registry.add_provider(&blame_luke_provider)",
            "TaxRegistrationProvider::new(",
            "registry.add_provider(&tax_provider)",
            "PredictionRegistrationProvider::from_ports(",
            "registry.add_provider(&prediction_provider)",
            "EnrichmentRegistrationProvider::new(",
            "registry.add_provider(&enrichment_provider)",
            "TriviaRegistrationProvider::new(",
            "trivia_provider.gateway_observer()",
            "blame_luke_provider.gateway_observer()",
            "registry.add_provider(&trivia_provider)",
            "ManaRegistrationProvider::new(",
            "registry.add_provider(&mana_provider)",
            "PlayerTriviaRegistrationProvider::new(",
            "registry.add_provider(&player_trivia_provider)",
            "ScoutRegistrationProvider::new(",
            "registry.add_provider(&scout_provider)",
            "WrappedRegistrationProvider::new(",
            "registry.add_provider(&wrapped_provider)",
            "ShopRegistrationProvider::new(",
            "registry.add_provider(&shop_provider)",
            "SurveyRegistrationProvider::new(",
            "survey_provider.gateway_observer()",
            "registry.add_provider(&survey_provider)",
            "survey_provider.recovery_worker_spec()",
            "with_worker(survey_recovery_worker)",
            "MafiaRegistrationProvider::new(",
            "mafia_provider.gateway_observer()",
            "registry.add_provider(&mafia_provider)",
            "mafia_provider.worker_spec(",
            "with_worker(mafia_phase_worker)",
            "PetRegistrationProvider::new(",
            "registry.add_provider(&pet_provider)",
            "pet_sweep_worker_spec_with_ai(",
            "runtime.with_worker(pet_sweep_worker)",
        ] {
            assert!(MAIN_SOURCE.contains(seam), "missing production seam {seam}");
        }
    }

    #[test]
    fn live_survey_provider_and_recovery_worker_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[28],
            wired(
                "commands.survey",
                "SurveyRegistrationProvider + nonce-safe DM/components + READY reconciliation + sliding result pager",
            )
        );
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[9],
            wired(
                "survey scheduled-delivery recovery",
                "SurveyRecoveryWorker + bounded delivery/reconciliation retries",
            )
        );
    }

    #[test]
    fn conditional_pet_sweep_worker_is_supervised_in_production() {
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[8],
            wired(
                "pet sweep (PET_CHANNEL_ID conditional)",
                "PetSweepWorker + stale-brawl-first durable lifecycle delivery + exact AI/fallback flavor + authored/native media + reminder/DM retry policy",
            )
        );
    }

    #[test]
    fn live_mafia_provider_and_phase_worker_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[25],
            wired(
                "commands.mafia",
                "MafiaRegistrationProvider + durable SQLite phase/setup/settlement recovery + private-thread Discord composition",
            )
        );
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[7],
            wired(
                "mafia phase loop",
                "MafiaPhaseWorker + five-minute guild sweep + durable reminder/phase/outbox recovery",
            )
        );
    }

    #[test]
    fn conditional_pet_provider_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[26],
            wired(
                "commands.pet (PET_CHANNEL_ID conditional)",
                "PetRegistrationProvider + conditional SQLite commands/components + brawl/eating/media/restart lifecycle",
            )
        );
    }

    #[test]
    fn live_match_and_admin_extensions_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[3],
            wired(
                "commands.match",
                "MatchRegistrationProvider + shared live lobby/enrichment/Discord runtime + durable pending-match READY recovery",
            )
        );
        assert_eq!(
            PYTHON_EXTENSIONS[4],
            wired(
                "commands.admin",
                "AdminRegistrationProvider + shared live Discord/lobby/match/OpenDota/UsageMonitor controls + durable correction reward/bet/OpenSkill READY recovery",
            )
        );
        assert_eq!(PYTHON_EXTENSIONS[5].status, CutoverStatus::Wired);
    }

    #[test]
    fn live_match_and_admin_composition_is_retained_by_production_main() {
        for seam in [
            "let usage_monitor = UsageMonitor::default()",
            "services.with_request_observer(",
            "GlobalInteractionHooks::new(usage_monitor)",
            "MatchRegistrationProvider::new(",
            "lobby_provider.match_lobby_port()",
            "enrichment_provider.recorded_match_discovery()",
            "match_provider.gateway_observer()",
            "match_provider.attach_reminder_hooks(reminder_provider.hooks())",
            "registry.add_provider(&match_provider)",
            "DraftRegistrationProvider::new_with_reminder_scheduler_and_neon(",
            "registration_provider.draft_neon_observer()",
            "registry.add_provider(&draft_provider)",
            "AdminMatchCorrectionRuntime::new(",
            "match_provider.correction_reward_control()",
            "AdminRegistrationProvider::new(",
            "AdminRuntimePorts {",
            "usage_monitor.clone()",
            "lobby_provider.admin_control()",
            "match_provider.admin_control()",
            "admin_match_correction.control()",
            "admin_match_correction.gateway_observer()",
            "registry.add_provider(&admin_provider)",
        ] {
            assert!(MAIN_SOURCE.contains(seam), "missing production seam {seam}");
        }
    }

    #[test]
    fn live_betting_provider_and_timeout_composition_are_retained_by_production_main() {
        assert_eq!(
            PYTHON_EXTENSIONS[5],
            wired(
                "commands.betting",
                "BettingRegistrationProvider + shared vanity-tax/wager-refresh/debrief/flavor composition + supervised live-view timeout",
            )
        );
        for seam in [
            "BettingRegistrationProvider::with_runtime_config_and_vanity_tax(",
            "BettingRuntimeConfig::from_application_config(",
            "Arc::clone(&vanity_tax_service)",
            "betting_provider.set_wager_refresh_port(match_wager_refresh_port(",
            ".attach_post_match_debrief(match_post_match_debrief_port(",
            "match_provider.attach_betting_flavor(production_betting_flavor(",
            "betting_provider.gateway_observer()",
            "registry.add_provider(&betting_provider)",
            "let betting_view_timeout_worker = betting_provider.timeout_worker()",
            ".with_worker(betting_view_timeout_worker)",
        ] {
            assert!(
                MAIN_SOURCE.contains(seam),
                "missing Betting production seam {seam}"
            );
        }
    }

    #[test]
    fn live_draft_is_promoted_without_promoting_adjacent_surfaces() {
        assert_eq!(
            PYTHON_EXTENSIONS[16],
            wired(
                "commands.draft",
                "DraftRegistrationProvider + shared lobby/draft state + Match reminder/Neon adapter composition",
            )
        );
        assert_eq!(PYTHON_EXTENSIONS[5].python_name, "commands.betting");
        assert_eq!(PYTHON_EXTENSIONS[5].status, CutoverStatus::Wired);
        for seam in [
            "DraftRegistrationProvider::new_with_reminder_scheduler_and_neon(",
            "registration_provider.draft_neon_observer()",
            "registry.add_provider(&draft_provider)",
        ] {
            assert!(
                MAIN_SOURCE.contains(seam),
                "missing Draft production seam {seam}"
            );
        }
    }

    #[test]
    fn live_dig_provider_bonus_components_and_recovery_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[24],
            wired(
                "commands.dig",
                "DigRegistrationProvider + durable SQLite mechanics/event outboxes + bonus components + READY reconciliation + native media/flavor hooks",
            )
        );
        for seam in [
            "DigBonusRuntime::from_application_config(",
            "trivia_provider.catalog()",
            "DigRegistrationProvider::production(",
            "dig_provider.gateway_observer()",
            "registry.add_provider(&dig_bonus_runtime)",
            "registry.add_provider(&dig_provider)",
            "validate_production_registry(&registry,",
        ] {
            assert!(
                MAIN_SOURCE.contains(seam),
                "missing Dig production seam {seam}"
            );
        }
    }

    #[test]
    fn live_info_commands_are_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[1],
            wired(
                "commands.info",
                "InfoRegistrationProvider + configured SQLite analytics + native statistical media",
            )
        );
    }

    #[test]
    fn live_rating_analysis_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[17],
            wired(
                "commands.rating_analysis",
                "RatingAnalysisRegistrationProvider + configured durable replay + native statistical media",
            )
        );
    }

    #[test]
    fn live_scout_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[19],
            wired(
                "commands.scout",
                "ScoutRegistrationProvider + shared live match/draft/lobby context + native paginated portrait media",
            )
        );
    }

    #[test]
    fn live_wrapped_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[20],
            wired(
                "commands.wrapped",
                "WrappedRegistrationProvider + complete SQLite story projections + native 16-page media lifecycle",
            )
        );
    }

    #[test]
    fn live_shop_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[9],
            wired(
                "commands.shop",
                "ShopRegistrationProvider + migrated SQLite atomic purchases/protection + live Serenity media/temporary-message delivery",
            )
        );
    }

    #[test]
    fn live_blame_luke_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[13],
            wired(
                "commands.blame_luke",
                "BlameLukeRegistrationProvider + atomic SQLite wallet/refund + native persistent GIF interaction",
            )
        );
    }

    #[test]
    fn live_tax_command_is_wired() {
        assert_eq!(
            PYTHON_EXTENSIONS[12],
            wired(
                "commands.tax",
                "TaxRegistrationProvider + migrated SQLite economy/vanity enforcement + scoped ephemeral pagers",
            )
        );
    }

    #[test]
    fn production_runtime_inventory_is_fully_wired() {
        assert_eq!(required_count() + wired_count(), 67);
        assert_eq!(required_count(), 0);
        assert_eq!(wired_count(), 67);
        assert!(all_items().all(|item| !item.rust_boundary.is_empty()));
    }

    #[test]
    fn production_global_command_replacement_requires_complete_inventory() {
        assert_eq!(required_count(), 0);
        assert!(MAIN_SOURCE.contains("inventory::global_command_sync_allowed("));
        assert!(MAIN_SOURCE.contains("inventory::required_count())"));
        assert!(MAIN_SOURCE.contains("registry.enable_global_command_sync();"));
    }

    #[test]
    fn complete_inventory_enables_global_command_replacement() {
        assert!(!global_command_sync_allowed(1));
        assert!(global_command_sync_allowed(0));
    }

    #[test]
    fn rust_entrypoint_import_is_side_effect_free_before_serve() {
        // Importing the Python module constructs a bot but deliberately does
        // not connect it. The Rust binary has the stronger equivalent: its
        // argument parser and inventory path are pure, while the Discord
        // gateway is reachable only through the explicit `serve` branch.
        assert!(MAIN_SOURCE.contains("match parse_command(env::args().skip(1))"));
        assert!(MAIN_SOURCE.contains("Ok(Command::Inventory) => {"));
        assert!(MAIN_SOURCE.contains("Ok(Command::Serve) => run_serve().await"));
        let main_body = MAIN_SOURCE
            .split_once("async fn run_serve()")
            .map(|(prefix, _)| prefix)
            .expect("run_serve definition");
        assert!(main_body.contains("match parse_command(env::args().skip(1))"));
        assert!(!main_body.contains("SerenityGateway::"));
    }

    fn assert_startup_failure_is_unpublished(marker: &str) {
        let marker_start = MAIN_SOURCE
            .find(marker)
            .unwrap_or_else(|| panic!("missing startup stage {marker}"));
        let runtime_start = MAIN_SOURCE
            .find("let mut runtime = Runtime::new(")
            .expect("runtime publication point");
        assert!(
            marker_start < runtime_start,
            "startup stage moved after runtime publication"
        );
        let stage = &MAIN_SOURCE[marker_start..runtime_start];
        assert!(
            stage.contains("return ExitCode::from(1)"),
            "startup stage {marker} must return before publishing a partial runtime"
        );
    }

    #[test]
    fn startup_constructor_failure_does_not_publish_runtime() {
        assert_startup_failure_is_unpublished(
            "let lobby_provider = match LobbyRegistrationProvider::new(",
        );
    }

    #[test]
    fn startup_initialize_failure_does_not_publish_runtime() {
        assert_startup_failure_is_unpublished(
            "let database_initialization = match SqliteDatabaseInitializer::with_migration_settings(",
        );
    }

    #[test]
    fn startup_monitoring_failure_does_not_publish_runtime() {
        assert_startup_failure_is_unpublished(
            "let health_reporter = match HealthReporter::initialize(",
        );
    }

    #[test]
    fn startup_expose_failure_does_not_publish_runtime() {
        assert_startup_failure_is_unpublished(
            "let vanity_tax_service = match service_container.components()",
        );
    }

    #[test]
    fn vanity_membership_gateway_and_recovery_are_production_wired() {
        assert!(
            PYTHON_GATEWAY_EVENTS
                .iter()
                .filter(|item| item.python_name.starts_with("on_member_"))
                .all(|item| item.status == CutoverStatus::Wired)
        );
        assert_eq!(
            PYTHON_READY_RECOVERY_ACTIONS[0],
            wired(
                "refresh vanity-tax membership snapshots",
                "paginated Serenity HTTP ready/resume recovery observer",
            )
        );
    }

    #[test]
    fn live_global_hooks_and_lobby_reactions_are_wired() {
        assert_eq!(
            &PYTHON_GATEWAY_EVENTS[6..10],
            &[
                wired(
                    "bot.tree.error",
                    "GlobalInteractionHooks typed policy + Serenity command dispatch",
                ),
                wired(
                    "on_interaction",
                    "UsageMonitor observer in live Serenity command dispatch",
                ),
                wired(
                    "on_raw_reaction_add",
                    "Serenity raw add dispatch + LobbyRawReactionObserver suspension/ready/Neon auxiliary composition",
                ),
                wired(
                    "on_raw_reaction_remove",
                    "Serenity raw remove dispatch + LobbyRawReactionObserver",
                ),
            ]
        );
    }

    #[test]
    fn live_lobby_extension_and_recovery_are_registered() {
        assert_eq!(
            PYTHON_EXTENSIONS[2],
            wired(
                "commands.lobby",
                "LobbyRegistrationProvider + durable SQLite moderation/audit + ready/Neon/registration-observer composition",
            )
        );
        assert_eq!(
            PYTHON_READY_RECOVERY_ACTIONS[1],
            wired(
                "reconcile persisted lobby messages",
                "LobbyGatewayObserver on Serenity ready/resume",
            )
        );
    }

    #[test]
    fn complete_python_configuration_surface_is_typed_and_production_consumable() {
        assert!(
            CONFIGURATION_DOMAINS
                .iter()
                .all(|item| item.status == CutoverStatus::Wired)
        );
    }

    #[test]
    fn configured_llm_transport_and_ask_consumer_are_production_wired() {
        assert_eq!(
            EXTERNAL_PROVIDERS[1],
            wired(
                "OpenAI-compatible LLM HTTP",
                "OpenAiCompatibleProvider + SQLite attempt telemetry",
            )
        );
        assert_eq!(
            PYTHON_EXTENSIONS[14],
            wired(
                "commands.ask",
                "configured AskRegistrationProvider + guild-scoped read-only SQLite/LLM adapter",
            )
        );
    }

    #[test]
    fn manashop_debt_worker_is_supervised_in_production() {
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[2],
            wired(
                "manashop_debt",
                "ManashopDebtWorker + atomic due Dark Bargain settlement",
            )
        );
    }

    #[test]
    fn duel_and_first_game_pool_workers_are_supervised_in_production() {
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[3],
            wired(
                "duel_challenges",
                "DuelChallengesWorker + durable claim/rearm/expiry delivery",
            )
        );
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[5],
            wired(
                "first_game_pool (amount conditional)",
                "FirstGamePoolWorker + atomic funding/display-refresh outbox",
            )
        );
    }

    #[test]
    fn economy_events_worker_is_conditionally_supervised_in_production() {
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[4],
            wired(
                "economy_events (ECONOMY_EVENTS_ENABLED conditional)",
                "EconomyEventsWorker + atomic activation and durable announcement stamps",
            )
        );
    }

    #[test]
    fn dig_weather_worker_is_supervised_in_production() {
        assert_eq!(
            PYTHON_BACKGROUND_TASKS[6],
            wired(
                "dig weather broadcast",
                "DigWeatherWorker + atomic two-layer daily forecast + live Discord rollover delivery",
            )
        );
        assert!(MAIN_SOURCE.contains("dig_weather_worker_spec("));
        assert!(MAIN_SOURCE.contains(".with_worker(dig_weather_worker)"));
    }

    #[test]
    fn prediction_workers_are_supervised_with_durable_publications() {
        assert_eq!(
            &PYTHON_BACKGROUND_TASKS[..2],
            &[
                wired(
                    "prediction_refresh",
                    "PredictionRefreshWorker + transactional publication outbox and live message refresh",
                ),
                wired(
                    "prediction_digest",
                    "PredictionDigestWorker + durable digest cursor/outbox and PNG delivery",
                ),
            ]
        );
    }
}
