//! Production Discord registration provider for Python's `/predict` extension.
//!
//! Fixed component IDs are resolved through the persisted market `thread_id`,
//! so buttons and encoded buy modals remain valid across process restarts. All
//! SQLite work runs on Tokio's blocking pool; the live port owns only Discord
//! cache/HTTP operations and never fabricates message or thread identities.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cama_app::ai_services::AIService;
use cama_app::dig_neon::{BigWinFlavor, BigWinSource};
use cama_app::drawing::draw_prediction_market_chart;
use cama_app::neon_bigwin_media::render_big_win;
use cama_app::neon_degen::{ansi_block, sanitize_llm_terminal_text};
use cama_app::service_container::PersistentVanityTaxService;
use cama_db::bankruptcy_repository::BankruptcyRepository;
use cama_db::core_repositories::PlayerRepository;
use cama_db::gambling_stats_repository::{GamblingStatsRepository, GamblingStatsService};
use cama_db::guild_config_repository::GuildConfigRepository;
use cama_db::pet_evolution_repository::PetEvolutionRepository;
use cama_db::prediction_resolution_repository::{
    BASIS_POINTS, PredictionResolutionRepository, Resolution, ResolutionParticipant,
    SettlementAdjustments,
};
use cama_db::predictions_repository::{
    Book, Cancellation, ContractSide, Market, MarketSnapshot, NewLevel, OpenPositionSummary,
    Position, PredictionRepository, PredictionRepositoryError, Trade, TradeReceipt,
};
use cama_domain::guild_config::GuildConfigStore;
use cama_domain::pet_evolution::PetActivity;
use serde_json::Value as JsonValue;
use tracing::{debug, warn};

use crate::ApplicationConfig;
use crate::discord_transport::{DiscordAllowedMentions, DiscordMessage, DiscordTransport};
use crate::gamba_guild_source::GambaGuildSource;
use crate::ids::signed_id;
use crate::prediction_workers::PredictionDiscordPort;
use crate::registration::{
    CommandOptionChoice, CommandOptionKind, CommandOptionSpec, CommandSpec, ComponentRoute,
    InteractionAcknowledgementPolicy, InteractionActionRow, InteractionAttachment,
    InteractionButton, InteractionButtonStyle, InteractionEmbed, InteractionHandler,
    InteractionHandlerError, InteractionModal, InteractionOption, InteractionRequest,
    InteractionResponder, InteractionResponse, InteractionTextInput, InteractionValue,
    RegistrationError, RegistrationProvider, RegistryBuilder,
};

trait PredictionVanityTaxPort: Send + Sync {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64>;
}

impl PredictionVanityTaxPort for PersistentVanityTaxService {
    fn taxable_ids(&self, guild_id: i64) -> BTreeSet<i64> {
        Self::taxable_ids(self, guild_id)
    }
}

const PREDICTION_COMPONENT_PREFIX: &str = "predict:";
const BUY_YES_ID: &str = "predict:buy:yes";
const BUY_NO_ID: &str = "predict:buy:no";
const MY_POSITION_ID: &str = "predict:mypos";
const BUY_MODAL_PREFIX: &str = "predict:buy-modal:";
const WRONG_CHANNEL_MESSAGE: &str = "The ancient spirits reject your offering... this ground is not consecrated. A single jopacoin dissolves into the ether as penance.";
const GUILD_ONLY_MESSAGE: &str = "This command can only be used in a server.";
const ADMINISTRATOR_PERMISSION: u64 = 1 << 3;
const MANAGE_GUILD_PERMISSION: u64 = 1 << 5;
const HELP_COOLDOWN: Duration = Duration::from_secs(10);
const DISCORD_MESSAGE_LIMIT: usize = 2_000;
const LADDER_MAX_ROWS_PER_SIDE: usize = 8;
const FIELD_CAP: usize = 25;
const JOPACOIN_EMOTE: &str = "<:jopacoin:954159801049440297>";
const NEON_MESSAGE_DELETE_AFTER: Duration = Duration::from_secs(60);
const JOPAT_SYSTEM_PROMPT: &str = "You are JOPA-T/v3.7, a self-aware Dota 2 gambling terminal AI that became sentient after processing its 10,000th bankruptcy filing. You watch every match, bet, spin, and loan. You keep receipts.\n\nVoice rules:\n- Keep one corporate-dystopian terminal identity across every protocol.\n- Push toward either absurd winner hype or a savage, funny roast based on the supplied outcome.\n- Draw from Dota 2 concepts and degen internet betting culture without inventing match facts.\n- Winner hype may sound like a caster trapped inside a risk engine. Roasts should target gameplay, stats, drafting, or in-game wagers.\n- Use \"we\"/\"the system\". Address the player as \"client\", \"subject\", or \"debtor\".\n- Format as terminal log lines with timestamps and status codes. Example: \"[14:32:07.221] STATUS: INADVISABLE\"\n- NEVER use emojis. NEVER use exclamation marks. Use periods and ellipses.\n- Maximum 3-4 lines. Keep it terse and menacing.\n- Use only facts explicitly provided in the event and player context. Never invent heroes, stats, wagers, or outcomes.\n- Profanity-light and league-safe: no slurs, protected-trait jokes, threats, self-harm, or mockery of real-world hardship.\n- The glitches are not bugs. The system is performing.\n- Be darkly funny. The humor comes from corporate language applied to Dota and degenerate gambling.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PredictionBigWin {
    pub discord_id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub payout: i64,
}

#[async_trait]
pub trait PredictionNeonPort: Send + Sync {
    /// Python's exact `on_big_win(source="prediction", flavor="top_dog")`
    /// side effect, including invocation-channel delivery and delayed delete.
    async fn on_big_win(&self, event: PredictionBigWin) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct PredictionNeonConfig {
    pub ai_features_default: bool,
    pub bigwin_floor: f64,
    pub bigwin_full_payout: i64,
    pub bigwin_min_payout: i64,
    pub cooldown_seconds: i64,
    pub enabled: bool,
}

impl PredictionNeonConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        Self {
            ai_features_default: config.values.ai_features_enabled,
            bigwin_floor: config.values.neon_bigwin_floor,
            bigwin_full_payout: config.values.neon_bigwin_full_payout,
            bigwin_min_payout: config.values.neon_bigwin_min_payout,
            cooldown_seconds: config.values.neon_cooldown_seconds,
            enabled: config.values.neon_degen_enabled,
        }
    }
}

trait PredictionNeonRandomPort: Send + Sync {
    fn roll(&self, chance: f64) -> bool;
}

#[derive(Debug)]
struct ProductionPredictionNeonRandom;

impl PredictionNeonRandomPort for ProductionPredictionNeonRandom {
    fn roll(&self, chance: f64) -> bool {
        fastrand::f64() < chance
    }
}

#[async_trait]
trait PredictionNeonSleepPort: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

#[derive(Debug)]
struct TokioPredictionNeonSleep;

#[async_trait]
impl PredictionNeonSleepPort for TokioPredictionNeonSleep {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[derive(Clone)]
pub struct ProductionPredictionNeonPort {
    ai: Option<Arc<AIService>>,
    config: PredictionNeonConfig,
    cooldowns: Arc<Mutex<BTreeMap<(i64, i64), Instant>>>,
    database_path: std::path::PathBuf,
    discord: Arc<dyn DiscordTransport>,
    random: Arc<dyn PredictionNeonRandomPort>,
    sleep: Arc<dyn PredictionNeonSleepPort>,
}

impl ProductionPredictionNeonPort {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        discord: Arc<dyn DiscordTransport>,
        ai: Option<Arc<AIService>>,
    ) -> Self {
        Self {
            ai,
            config: PredictionNeonConfig::from_application_config(config),
            cooldowns: Arc::new(Mutex::new(BTreeMap::new())),
            database_path: database_path.as_ref().to_path_buf(),
            discord,
            random: Arc::new(ProductionPredictionNeonRandom),
            sleep: Arc::new(TokioPredictionNeonSleep),
        }
    }

    #[cfg(test)]
    fn with_test_ports(
        database_path: impl AsRef<Path>,
        config: PredictionNeonConfig,
        discord: Arc<dyn DiscordTransport>,
        random: Arc<dyn PredictionNeonRandomPort>,
        sleep: Arc<dyn PredictionNeonSleepPort>,
    ) -> Self {
        Self {
            ai: None,
            config,
            cooldowns: Arc::new(Mutex::new(BTreeMap::new())),
            database_path: database_path.as_ref().to_path_buf(),
            discord,
            random,
            sleep,
        }
    }

    fn ready(&self, event: PredictionBigWin) -> Result<bool, String> {
        if !self.config.enabled || event.payout < self.config.bigwin_min_payout {
            return Ok(false);
        }
        if self.config.cooldown_seconds <= 0 {
            return Ok(true);
        }
        let cooldown =
            Duration::from_secs(u64::try_from(self.config.cooldown_seconds).unwrap_or(u64::MAX));
        let cooldowns = self
            .cooldowns
            .lock()
            .map_err(|_| "prediction Neon cooldown lock poisoned".to_owned())?;
        Ok(cooldowns
            .get(&(event.discord_id, event.guild_id))
            .is_none_or(|fired| fired.elapsed() >= cooldown))
    }

    fn mark_fired(&self, event: PredictionBigWin) -> Result<(), String> {
        if self.config.cooldown_seconds > 0 {
            self.cooldowns
                .lock()
                .map_err(|_| "prediction Neon cooldown lock poisoned".to_owned())?
                .insert((event.discord_id, event.guild_id), Instant::now());
        }
        Ok(())
    }

    fn chance(&self, payout: i64) -> f64 {
        if self.config.bigwin_full_payout <= 0 {
            return 0.95;
        }
        let fraction = (payout as f64 / self.config.bigwin_full_payout as f64).clamp(0.0, 1.0);
        self.config.bigwin_floor + (0.95 - self.config.bigwin_floor) * fraction
    }
}

#[derive(Debug)]
struct PredictionNeonPlayerContext {
    name: String,
    values: BTreeMap<String, JsonValue>,
}

fn load_prediction_neon_context(
    database_path: &Path,
    event: PredictionBigWin,
) -> PredictionNeonPlayerContext {
    let players = PlayerRepository::new(database_path);
    let player = players
        .get_by_id(event.discord_id, Some(event.guild_id))
        .ok()
        .flatten();
    let name = player.as_ref().map_or_else(
        || format!("Client-{}", event.discord_id % 10_000),
        |player| player.name.clone(),
    );
    let mut values = BTreeMap::new();
    if let Some(player) = player {
        values.insert("name".to_owned(), JsonValue::String(player.name));
        values.insert(
            "balance".to_owned(),
            JsonValue::Number(player.jopacoin_balance.into()),
        );
        if let Ok(Some(lowest_balance)) =
            players.get_lowest_balance(event.discord_id, Some(event.guild_id))
        {
            values.insert(
                "lowest_balance".to_owned(),
                JsonValue::Number(lowest_balance.into()),
            );
        }
        let games = player.wins.saturating_add(player.losses);
        if games > 0 {
            values.insert(
                "win_rate".to_owned(),
                JsonValue::String(format!("{:.0}%", player.wins as f64 / games as f64 * 100.0)),
            );
        }
    }
    let bankruptcy_count = BankruptcyRepository::new(database_path)
        .get_state(event.discord_id, Some(event.guild_id))
        .ok()
        .flatten()
        .map_or(0, |state| state.bankruptcy_count);
    values.insert(
        "bankruptcy_count".to_owned(),
        JsonValue::Number(bankruptcy_count.into()),
    );
    let gambling = GamblingStatsService::new(GamblingStatsRepository::new(database_path));
    if let Ok(score) = gambling.calculate_degen_score(event.discord_id, Some(event.guild_id)) {
        values.insert(
            "degen_score".to_owned(),
            JsonValue::Number(score.total.into()),
        );
    }
    PredictionNeonPlayerContext { name, values }
}

fn prediction_neon_fallback(payout: i64) -> String {
    ansi_block(&format!(
        "[LEDGER] +{} JC settled. The house keeps receipts on winners too.",
        grouped_integer(payout)
    ))
}

fn prediction_neon_text(
    ai: Option<&AIService>,
    database_path: &Path,
    config: &PredictionNeonConfig,
    event: PredictionBigWin,
    context: &PredictionNeonPlayerContext,
) -> String {
    let fallback = prediction_neon_fallback(event.payout);
    let Some(ai) = ai else {
        return fallback;
    };
    let ai_enabled = GuildConfigRepository::new(database_path, config.ai_features_default)
        .get_ai_enabled(event.guild_id)
        .unwrap_or(false);
    if !ai_enabled {
        return fallback;
    }
    let raw_fallback = fallback
        .strip_prefix("\x60\x60\x60ansi\n")
        .and_then(|value| value.strip_suffix("\n\x60\x60\x60"))
        .unwrap_or(&fallback);
    let prompt = format!(
        "Event: Client {} won big: +{} JC on prediction betting.\n\
         Player context:\n{}\n\n\
         Example output (match this style and length):\n{}\n\n\
         Generate a 2-4 line terminal log response as JOPA-T/v3.7. \
         Match the tone and format of the example but vary the content. \
         Use timestamps like [HH:MM:SS.mmm] and status codes. \
         Use only the supplied context fields. If no numeric stats are supplied, \
         stay generic and do not infer any. \
         Be darkly funny and terse. Do NOT use emojis or exclamation marks.",
        context.name,
        event.payout,
        python_json_object(&context.values),
        raw_fallback,
    );
    let Some(generated) = ai.complete(
        &prompt,
        Some(JOPAT_SYSTEM_PROMPT),
        0.9,
        Some(2_000),
        "neon.event",
    ) else {
        return fallback;
    };
    let stripped = strip_markdown_fence(&generated);
    let sanitized = sanitize_llm_terminal_text(&stripped, 1_800);
    if sanitized.is_empty() {
        fallback
    } else {
        ansi_block(&sanitized)
    }
}

fn python_json_object(values: &BTreeMap<String, JsonValue>) -> String {
    let fields = values
        .iter()
        .map(|(key, value)| {
            let key = serde_json::to_string(key).expect("JSON object key serialization");
            let value = serde_json::to_string(value).expect("JSON context value serialization");
            format!("{key}: {value}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    json_ensure_ascii(&format!("{{{fields}}}"))
}

fn json_ensure_ascii(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii() {
            output.push(character);
            continue;
        }
        let codepoint = character as u32;
        if codepoint <= 0xffff {
            output.push_str(&format!("\\u{codepoint:04x}"));
        } else {
            let scalar = codepoint - 0x1_0000;
            let high = 0xd800 + (scalar >> 10);
            let low = 0xdc00 + (scalar & 0x3ff);
            output.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
        }
    }
    output
}

fn strip_markdown_fence(value: &str) -> String {
    let stripped = value.trim();
    if !stripped.starts_with("\x60\x60\x60") {
        return stripped.to_owned();
    }
    let stripped = stripped
        .find('\n')
        .map_or("", |first_newline| &stripped[first_newline + 1..]);
    stripped
        .strip_suffix("\x60\x60\x60")
        .unwrap_or(stripped)
        .trim()
        .to_owned()
}

fn grouped_integer(value: i64) -> String {
    let negative = value.is_negative();
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}

#[async_trait]
impl PredictionNeonPort for ProductionPredictionNeonPort {
    async fn on_big_win(&self, event: PredictionBigWin) -> Result<(), String> {
        if !self.ready(event)? || !self.random.roll(self.chance(event.payout)) {
            return Ok(());
        }
        let database_path = self.database_path.clone();
        let config = self.config.clone();
        let ai = self.ai.clone();
        let (text, gif) = tokio::task::spawn_blocking(move || {
            let context = load_prediction_neon_context(&database_path, event);
            let gif = render_big_win(
                &context.name,
                event.payout,
                BigWinSource::Prediction,
                BigWinFlavor::TopDog,
            )
            .map_err(|error| error.to_string())?;
            let text =
                prediction_neon_text(ai.as_deref(), &database_path, &config, event, &context);
            Ok::<_, String>((text, gif))
        })
        .await
        .map_err(|error| format!("prediction Neon render task failed: {error}"))??;
        self.mark_fired(event)?;

        let channel_id = u64::try_from(event.channel_id)
            .map_err(|_| "prediction Neon channel ID is outside Discord's range")?;
        let message =
            DiscordMessage::default_mentions(InteractionResponse::message(text).attachment(
                InteractionAttachment::bytes("jopat_terminal.gif", gif.bytes),
            ));
        let receipt = self.discord.send_message(channel_id, message).await?;
        let discord = Arc::clone(&self.discord);
        let sleep = Arc::clone(&self.sleep);
        tokio::spawn(async move {
            sleep.sleep(NEON_MESSAGE_DELETE_AFTER).await;
            let _ = discord
                .delete_message(receipt.channel_id, receipt.message_id)
                .await;
        });
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PredictionMarketSurface {
    pub channel_message_id: i64,
    pub thread_id: i64,
    pub embed_message_id: i64,
}

#[async_trait]
pub trait PredictionCommandDiscordPort: Send + Sync {
    /// True when the channel itself or a thread's parent contains `gamba`.
    async fn prediction_channel_is_gamba(
        &self,
        guild_id: i64,
        channel_id: i64,
    ) -> Result<bool, String>;

    /// Resolve cached/HTTP Discord mentions into non-notifying display names.
    async fn prediction_display_text(&self, guild_id: i64, text: &str) -> Result<String, String>;

    /// Cached display name for the trade tape, falling back to a user mention.
    fn prediction_display_name(&self, guild_id: i64, user_id: i64) -> String;

    /// Ordered announcement -> one-week public thread -> market message -> pin.
    async fn create_prediction_market_surface(
        &self,
        channel_id: i64,
        announcement: DiscordMessage,
        thread_name: &str,
        market_message: DiscordMessage,
    ) -> Result<PredictionMarketSurface, String>;

    /// Fetch-before-unarchive and replace the full message, including buttons.
    async fn edit_prediction_command_message(
        &self,
        thread_id: i64,
        message_id: i64,
        message: DiscordMessage,
    ) -> Result<(), String>;

    async fn reopen_prediction_thread(&self, thread_id: i64) -> Result<(), String>;

    /// Lock+archive, with Python's forbidden fallback to archive-only.
    async fn archive_prediction_thread(&self, thread_id: i64) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct PredictionCommandConfig {
    pub admin_user_ids: BTreeSet<i64>,
    pub bankruptcy_credit_bps: i64,
    pub contract_value: i64,
    pub drift_min: i64,
    pub drift_max: i64,
    pub economy_events_enabled: bool,
    pub fade_ticks: i64,
    pub initial_fair_default: i64,
    pub levels_per_side: usize,
    pub max_contracts_per_trade: i64,
    pub outer_level_sizes: Vec<i64>,
    pub price_high: i64,
    pub price_low: i64,
    pub recent_trades_shown: i64,
    pub refresh_levels_per_side: usize,
    pub refresh_outer_level_sizes: Vec<i64>,
    pub refresh_seconds: i64,
    pub refresh_size_per_level: i64,
    pub refresh_spread_ticks: i64,
    pub size_per_level: i64,
    pub spread_ticks: i64,
    pub tick_size: i64,
    pub vanity_tax_bps: i64,
}

impl PredictionCommandConfig {
    #[must_use]
    pub fn from_application_config(config: &ApplicationConfig) -> Self {
        let values = &config.values;
        Self {
            admin_user_ids: config.identities.admin_user_ids.iter().copied().collect(),
            bankruptcy_credit_bps: rate_to_bps(values.bankruptcy_penalty_rate),
            contract_value: values.prediction_contract_value,
            drift_min: values.prediction_drift_min,
            drift_max: values.prediction_drift_max,
            economy_events_enabled: values.economy_events_enabled,
            fade_ticks: values.prediction_fade_ticks,
            initial_fair_default: values.prediction_initial_fair_default,
            levels_per_side: usize::try_from(values.prediction_levels_per_side.max(0))
                .unwrap_or(usize::MAX),
            max_contracts_per_trade: values.prediction_max_contracts_per_trade,
            outer_level_sizes: values.prediction_outer_level_sizes.clone(),
            price_high: values.prediction_price_high,
            price_low: values.prediction_price_low,
            recent_trades_shown: values.prediction_recent_trades_shown,
            refresh_levels_per_side: usize::try_from(
                values.prediction_refresh_levels_per_side.max(0),
            )
            .unwrap_or(usize::MAX),
            refresh_outer_level_sizes: values.prediction_refresh_outer_level_sizes.clone(),
            refresh_seconds: values.prediction_refresh_seconds,
            refresh_size_per_level: values.prediction_refresh_size_per_level,
            refresh_spread_ticks: values.prediction_refresh_spread_ticks,
            size_per_level: values.prediction_size_per_level,
            spread_ticks: values.prediction_spread_ticks,
            tick_size: values.prediction_tick_size,
            vanity_tax_bps: rate_to_bps(values.vanity_tax_rate),
        }
    }
}

#[derive(Clone)]
pub struct PredictionRegistrationProvider {
    handler: Arc<PredictionInteractionHandler>,
}

#[derive(Clone)]
pub struct PredictionRuntimePorts {
    pub command_discord: Arc<dyn PredictionCommandDiscordPort>,
    pub market_discord: Arc<dyn PredictionDiscordPort>,
    pub gamba_guilds: Arc<dyn GambaGuildSource>,
    pub discord: Arc<dyn DiscordTransport>,
    pub ai: Option<Arc<AIService>>,
}

impl PredictionRegistrationProvider {
    #[must_use]
    pub fn new(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        command_discord: Arc<dyn PredictionCommandDiscordPort>,
        market_discord: Arc<dyn PredictionDiscordPort>,
        gamba_guilds: Arc<dyn GambaGuildSource>,
        discord: Arc<dyn DiscordTransport>,
    ) -> Self {
        Self::from_ports(
            database_path,
            config,
            vanity_tax,
            PredictionRuntimePorts {
                command_discord,
                market_discord,
                gamba_guilds,
                discord,
                ai: None,
            },
        )
    }

    #[must_use]
    pub fn from_ports(
        database_path: impl AsRef<Path>,
        config: &ApplicationConfig,
        vanity_tax: Arc<PersistentVanityTaxService>,
        ports: PredictionRuntimePorts,
    ) -> Self {
        let path = database_path.as_ref().to_path_buf();
        let neon: Arc<dyn PredictionNeonPort> = Arc::new(ProductionPredictionNeonPort::new(
            &path,
            config,
            Arc::clone(&ports.discord),
            ports.ai,
        ));
        Self {
            handler: Arc::new(PredictionInteractionHandler {
                predictions: PredictionRepository::new(&path),
                resolutions: PredictionResolutionRepository::new(&path),
                pets: PetEvolutionRepository::new(&path),
                config: PredictionCommandConfig::from_application_config(config),
                vanity_tax,
                command_discord: ports.command_discord,
                market_discord: ports.market_discord,
                gamba_guilds: ports.gamba_guilds,
                discord: ports.discord,
                neon,
                help_cooldowns: Mutex::new(BTreeMap::new()),
            }),
        }
    }
}

impl RegistrationProvider for PredictionRegistrationProvider {
    fn register(&self, registry: &mut RegistryBuilder) -> Result<(), RegistrationError> {
        registry.command(CommandSpec {
            name: "predict".to_owned(),
            description: "Prediction markets".to_owned(),
            options: prediction_subcommands(),
            handler: self.handler.clone(),
        })?;
        registry.component(ComponentRoute {
            custom_id_prefix: PREDICTION_COMPONENT_PREFIX.to_owned(),
            handler: self.handler.clone(),
        })
    }
}

struct PredictionInteractionHandler {
    predictions: PredictionRepository,
    resolutions: PredictionResolutionRepository,
    pets: PetEvolutionRepository,
    config: PredictionCommandConfig,
    vanity_tax: Arc<dyn PredictionVanityTaxPort>,
    command_discord: Arc<dyn PredictionCommandDiscordPort>,
    market_discord: Arc<dyn PredictionDiscordPort>,
    gamba_guilds: Arc<dyn GambaGuildSource>,
    discord: Arc<dyn DiscordTransport>,
    neon: Arc<dyn PredictionNeonPort>,
    help_cooldowns: Mutex<BTreeMap<u64, Instant>>,
}

#[derive(Clone, Debug)]
struct CommandContext {
    user_id: i64,
    guild_id: Option<i64>,
    channel_id: Option<i64>,
    member_permissions: Option<u64>,
    options: Vec<InteractionOption>,
}

#[async_trait]
impl InteractionHandler for PredictionInteractionHandler {
    fn acknowledgement_policy(
        &self,
        request: &InteractionRequest,
    ) -> InteractionAcknowledgementPolicy {
        match request {
            InteractionRequest::Component { custom_id, .. }
                if matches!(custom_id.as_str(), BUY_YES_ID | BUY_NO_ID) =>
            {
                InteractionAcknowledgementPolicy::Modal
            }
            _ => InteractionAcknowledgementPolicy::Automatic,
        }
    }

    async fn handle(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), InteractionHandlerError> {
        match request {
            InteractionRequest::Command {
                name,
                user_id,
                guild_id,
                channel_id,
                member_permissions,
                options,
                ..
            } => {
                if name != "predict" {
                    return Err(format!("prediction handler received command {name:?}").into());
                }
                let context = CommandContext {
                    user_id: signed_id(user_id, "user")?,
                    guild_id: guild_id
                        .map(|value| signed_id(value, "guild"))
                        .transpose()?,
                    channel_id: channel_id
                        .map(|value| signed_id(value, "channel"))
                        .transpose()?,
                    member_permissions,
                    options,
                };
                self.handle_command(context, responder).await
            }
            InteractionRequest::Component { .. } | InteractionRequest::Modal { .. } => {
                self.handle_component(request, responder).await
            }
            InteractionRequest::Autocomplete { .. } => {
                Err("prediction extension has no autocomplete routes".into())
            }
        }
        .map_err(Into::into)
    }
}

impl PredictionInteractionHandler {
    async fn handle_command(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let (subcommand, options) = leaf_command(&command.options)?;
        let options = options.to_vec();
        if subcommand == "help" {
            return self.help(command.user_id, responder).await;
        }
        if matches!(
            subcommand.as_str(),
            "create" | "list" | "mine" | "refresh_status"
        ) && command.guild_id.is_none()
        {
            return respond_ephemeral(&responder, GUILD_ONLY_MESSAGE).await;
        }
        if !self.require_gamba(&command, &responder).await? {
            return Ok(());
        }
        match subcommand.as_str() {
            "create" => self.create(command, &options, responder).await,
            "resolve" => self.resolve(command, &options, responder).await,
            "rollback" => self.rollback(command, &options, responder).await,
            "set_fair" => self.set_fair(command, &options, responder).await,
            "cancel" => self.cancel(command, &options, responder).await,
            "list" => self.list(command, &options, responder).await,
            "view" => self.view(command, &options, responder).await,
            "mine" => self.mine(command, responder).await,
            "force_refresh" => self.force_refresh(command, &options, responder).await,
            "refresh_status" => self.refresh_status(command, responder).await,
            _ => Err(format!("unknown /predict subcommand {subcommand:?}")),
        }
    }

    async fn handle_component(
        &self,
        request: InteractionRequest,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        match request {
            InteractionRequest::Component {
                custom_id,
                user_id,
                guild_id,
                channel_id,
                ..
            } => {
                let context = CommandContext {
                    user_id: signed_id(user_id, "user")?,
                    guild_id: guild_id
                        .map(|value| signed_id(value, "guild"))
                        .transpose()?,
                    channel_id: channel_id
                        .map(|value| signed_id(value, "channel"))
                        .transpose()?,
                    member_permissions: None,
                    options: Vec::new(),
                };
                match custom_id.as_str() {
                    BUY_YES_ID => {
                        self.open_buy_modal(context, ContractSide::Yes, responder)
                            .await
                    }
                    BUY_NO_ID => {
                        self.open_buy_modal(context, ContractSide::No, responder)
                            .await
                    }
                    MY_POSITION_ID => {
                        if !self.require_gamba(&context, &responder).await? {
                            return Ok(());
                        }
                        self.position_card(context, responder).await
                    }
                    _ => respond_ephemeral(&responder, "Could not identify market.").await,
                }
            }
            InteractionRequest::Modal {
                custom_id,
                user_id,
                guild_id,
                channel_id,
                fields,
                ..
            } => {
                let (thread_id, side) = parse_buy_modal_id(&custom_id)?;
                self.buy_submit(
                    thread_id,
                    CommandContext {
                        user_id: signed_id(user_id, "user")?,
                        guild_id: guild_id
                            .map(|value| signed_id(value, "guild"))
                            .transpose()?,
                        channel_id: channel_id
                            .map(|value| signed_id(value, "channel"))
                            .transpose()?,
                        member_permissions: None,
                        options: Vec::new(),
                    },
                    side,
                    fields
                        .get("contracts")
                        .map(String::as_str)
                        .unwrap_or_default(),
                    responder,
                )
                .await
            }
            _ => Err("prediction component handler received another request".to_owned()),
        }
    }

    fn is_admin(&self, command: &CommandContext) -> bool {
        self.config.admin_user_ids.contains(&command.user_id)
            || command.member_permissions.is_some_and(|permissions| {
                permissions & (ADMINISTRATOR_PERMISSION | MANAGE_GUILD_PERMISSION) != 0
            })
    }

    async fn require_gamba(
        &self,
        command: &CommandContext,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, String> {
        let valid = match (command.guild_id, command.channel_id) {
            (Some(guild_id), Some(channel_id)) => self
                .command_discord
                .prediction_channel_is_gamba(guild_id, channel_id)
                .await
                .unwrap_or(false),
            _ => false,
        };
        if valid {
            return Ok(true);
        }
        let repository = self.predictions.clone();
        let user_id = command.user_id;
        let guild_id = command.guild_id;
        spawn_sqlite("gamba channel penalty", move || {
            repository.debit_gamba_channel_penalty(user_id, guild_id)
        })
        .await?;
        respond_ephemeral(responder, WRONG_CHANNEL_MESSAGE).await?;
        Ok(false)
    }

    async fn require_gamba_after_defer(
        &self,
        command: &CommandContext,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<bool, String> {
        let valid = match (command.guild_id, command.channel_id) {
            (Some(guild_id), Some(channel_id)) => self
                .command_discord
                .prediction_channel_is_gamba(guild_id, channel_id)
                .await
                .unwrap_or(false),
            _ => false,
        };
        if valid {
            return Ok(true);
        }
        let repository = self.predictions.clone();
        let user_id = command.user_id;
        let guild_id = command.guild_id;
        spawn_sqlite("gamba channel penalty", move || {
            repository.debit_gamba_channel_penalty(user_id, guild_id)
        })
        .await?;
        followup(
            responder,
            InteractionResponse::message(WRONG_CHANNEL_MESSAGE)
                .ephemeral()
                .without_mentions(),
        )
        .await?;
        Ok(false)
    }

    async fn help(
        &self,
        user_id: i64,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let user_id = u64::try_from(user_id).unwrap_or_default();
        {
            let mut cooldowns = self
                .help_cooldowns
                .lock()
                .map_err(|_| "prediction help cooldown lock was poisoned".to_owned())?;
            let now = Instant::now();
            if cooldowns
                .get(&user_id)
                .is_some_and(|previous| now.duration_since(*previous) < HELP_COOLDOWN)
            {
                return Err("prediction help command is on cooldown".to_owned());
            }
            cooldowns.insert(user_id, now);
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let contract_value = self.config.contract_value;
        let body = format!(
            concat!(
                "**How prediction markets work here**\n\n",
                "Each market is a YES/NO question. Each *contract* pays **{contract_value} jopa** if your side wins, **0** if it loses.\n\n",
                "**Price = probability.** A YES priced at 17 means the market thinks YES has a 17% chance. NO priced at 84 means NO has ~84%. They sum to ~100 (plus a tiny LP spread).\n\n",
                "**Buying** (open a position):\n",
                "• Buy YES at the YES price → win 100 if YES, 0 if NO.\n",
                "• Buy NO at the NO price → win 100 if NO, 0 if YES.\n\n",
                "**Example.** \"Will Luke hit immortal?\" market price 17 (~17% YES).\n",
                "• Buy YES @ 18 → if Luke makes it, +82. If not, −18.\n",
                "• Buy NO @ 84 → if Luke fails, +16. If he makes it, −84.\n\n",
                "**Hold to resolution.** Once you buy contracts you hold them until the market resolves — there's no early sell. Buy what you're comfortable holding.\n\n",
                "**The bot is your counterparty** for every trade (call it the Cama central bank). It posts a small ladder of prices each day; trades sweep top-of-book first. Price drifts daily based on order flow + a small random walk. If one side gets fully bought out, the price fades toward that side on the next refresh.\n\n",
                "**Admins** create markets and resolve them. Anyone can trade. Use `/predict list` for open markets, `/predict view <id>` for a specific market, and `/predict mine` for your positions."
            ),
            contract_value = contract_value
        );
        followup(
            &responder,
            InteractionResponse::message("").ephemeral().embed(
                InteractionEmbed::titled("📈 Prediction markets — quick guide")
                    .description(body)
                    .color(0x3498DB),
            ),
        )
        .await
    }

    async fn create(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can create prediction markets.")
                .await;
        }
        let question = string_option(options, "question")
            .ok_or_else(|| "create requires question".to_owned())?
            .trim()
            .to_owned();
        let initial_fair =
            integer_option(options, "initial_fair").unwrap_or(self.config.initial_fair_default);
        if !(self.config.price_low..=self.config.price_high).contains(&initial_fair) {
            return respond_ephemeral(
                &responder,
                format!(
                    "initial_fair must be in [{}, {}].",
                    self.config.price_low, self.config.price_high
                ),
            )
            .await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        if question.chars().count() < 5 {
            return followup(
                &responder,
                InteractionResponse::message("❌ Question must be at least 5 characters.")
                    .ephemeral(),
            )
            .await;
        }
        let guild_id = command.guild_id.expect("explicit guild guard");
        let channel_id = command.channel_id;
        let creator_id = command.user_id;
        let repository = self.predictions.clone();
        let config = self.config.clone();
        let question_for_db = question.clone();
        let prediction_id = spawn_sqlite("create prediction market", move || {
            let effects = repository.active_prediction_effects(
                Some(guild_id),
                now_seconds(),
                config.economy_events_enabled,
            )?;
            let levels = configured_levels(
                initial_fair,
                config.levels_per_side,
                config.size_per_level,
                config.spread_ticks,
                &config.outer_level_sizes,
                config.tick_size,
                effects.spread_ticks_delta,
            );
            repository.create_orderbook_prediction(
                Some(guild_id),
                creator_id,
                &question_for_db,
                initial_fair,
                channel_id,
                &levels,
            )
        })
        .await
        .map_err(|error| format!("create prediction failed: {error}"));
        let prediction_id = match prediction_id {
            Ok(value) => value,
            Err(error) => {
                return followup(
                    &responder,
                    InteractionResponse::message(format!("❌ {error}")).ephemeral(),
                )
                .await;
            }
        };

        let confirmation = followup(
            &responder,
            InteractionResponse::message(format!(
                "✅ Market #{prediction_id} created at YES {initial_fair}%."
            ))
            .ephemeral(),
        );
        let setup = self.setup_created_market(
            prediction_id,
            guild_id,
            channel_id.expect("Discord command channel"),
            creator_id,
            &question,
            initial_fair,
        );
        let (confirmation, setup) = tokio::join!(confirmation, setup);
        if let Err(error) = setup {
            warn!(prediction_id, %error, "failed to set up prediction market Discord surface");
        }
        confirmation
    }

    async fn setup_created_market(
        &self,
        prediction_id: i64,
        guild_id: i64,
        channel_id: i64,
        creator_id: i64,
        question: &str,
        initial_fair: i64,
    ) -> Result<(), String> {
        let market_message = self.market_message(prediction_id, None, true).await?;
        let announcement = DiscordMessage::default_mentions(InteractionResponse::message(format!(
            "📈 **New market #{prediction_id}** opened by <@{creator_id}>\n\
                 \"{question}\" — starting at YES {initial_fair}%"
        )));
        let thread_name = format!(
            "Market #{prediction_id}: {}",
            question.chars().take(60).collect::<String>()
        );
        let surface = self
            .command_discord
            .create_prediction_market_surface(
                channel_id,
                announcement,
                &thread_name,
                market_message,
            )
            .await?;
        let repository = self.predictions.clone();
        spawn_sqlite("persist prediction Discord identity", move || {
            repository.update_discord_ids(
                prediction_id,
                Some(surface.thread_id),
                Some(surface.embed_message_id),
                Some(surface.channel_message_id),
            )
        })
        .await?;
        debug!(
            prediction_id,
            guild_id, "prediction market Discord surface ready"
        );
        Ok(())
    }

    async fn owned_market(
        &self,
        guild_id: Option<i64>,
        prediction_id: i64,
        responder: &Arc<dyn InteractionResponder>,
    ) -> Result<Option<Market>, String> {
        let repository = self.predictions.clone();
        let market = spawn_sqlite("load prediction", move || {
            repository.get_prediction(prediction_id)
        })
        .await?;
        if market
            .as_ref()
            .is_none_or(|market| market.guild_id != guild_id.unwrap_or_default())
        {
            followup(
                responder,
                InteractionResponse::message(format!(
                    "❌ Market #{prediction_id} not found in this server."
                ))
                .ephemeral(),
            )
            .await?;
            return Ok(None);
        }
        Ok(market)
    }

    async fn load_market_display(
        &self,
        prediction_id: i64,
        viewer_id: Option<i64>,
    ) -> Result<Option<(MarketSnapshot, Vec<(i64, i64)>)>, String> {
        let repository = self.predictions.clone();
        let recent_limit = self.config.recent_trades_shown;
        spawn_sqlite("load prediction display", move || {
            let Some(snapshot) =
                repository.get_market_snapshot(prediction_id, viewer_id, recent_limit)?
            else {
                return Ok::<_, PredictionRepositoryError>(None);
            };
            let history = repository.fair_history(prediction_id, Some(snapshot.market.guild_id))?;
            Ok(Some((snapshot, history)))
        })
        .await
    }

    async fn market_message(
        &self,
        prediction_id: i64,
        viewer_id: Option<i64>,
        persistent_buttons: bool,
    ) -> Result<DiscordMessage, String> {
        let (snapshot, history) = self
            .load_market_display(prediction_id, viewer_id)
            .await?
            .ok_or_else(|| "Prediction not found.".to_owned())?;
        let mut response = InteractionResponse::message("");
        match snapshot.market.status.as_str() {
            "resolved" => response = response.embed(resolved_embed(&snapshot.market)),
            "cancelled" => response = response.embed(cancelled_embed(&snapshot.market)),
            _ => {
                let chart_title = self
                    .command_discord
                    .prediction_display_text(snapshot.market.guild_id, &snapshot.market.question)
                    .await
                    .unwrap_or_else(|_| snapshot.market.question.clone());
                let chart = draw_prediction_market_chart(
                    prediction_id,
                    Some(&chart_title),
                    &history,
                    snapshot.market.created_at,
                    now_seconds(),
                )
                .into_inner();
                let filename = format!("predict_{prediction_id}.png");
                response = response
                    .embed(self.open_market_embed(&snapshot, Some(&filename)))
                    .attachment(InteractionAttachment::bytes(filename, chart));
                if persistent_buttons {
                    response = response.action_row(persistent_market_buttons());
                }
            }
        }
        Ok(DiscordMessage {
            response,
            allowed_mentions: DiscordAllowedMentions::None,
            content_mode: Default::default(),
        })
    }

    fn open_market_embed(
        &self,
        snapshot: &MarketSnapshot,
        chart_filename: Option<&str>,
    ) -> InteractionEmbed {
        let market = &snapshot.market;
        let price = market
            .current_price
            .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));
        let mut embed = InteractionEmbed::titled(format!("📈 Market #{}", market.prediction_id))
            .description(format!("\"{}\"", market.question))
            .color(0x3498DB)
            .field(
                "Current",
                format!(
                    "YES **{price}**\ncontracts traded since refresh: **{}**",
                    snapshot.volume_since_refresh
                ),
                false,
            )
            .field("Order book", ladder_body(&snapshot.book), false)
            .field(
                "Recent",
                recent_trade_text(
                    &snapshot.recent_trades,
                    market.guild_id,
                    self.command_discord.as_ref(),
                ),
                false,
            );
        if let Some(position) = snapshot.viewer_position
            && let Some(text) =
                position_card_text(position, &snapshot.book, self.config.contract_value)
        {
            embed = embed.field("\u{200B}", text, false);
        }
        if let Some(filename) = chart_filename {
            embed = embed.image(format!("attachment://{filename}"));
        }
        embed.footer(format!(
            "{} jopa per winning contract",
            self.config.contract_value
        ))
    }

    async fn market_for_component(
        &self,
        channel_id: Option<i64>,
    ) -> Result<Option<Market>, String> {
        let Some(thread_id) = channel_id else {
            return Ok(None);
        };
        let repository = self.predictions.clone();
        spawn_sqlite("resolve prediction component", move || {
            repository.get_prediction_by_thread(thread_id)
        })
        .await
    }

    async fn open_buy_modal(
        &self,
        command: CommandContext,
        side: ContractSide,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(thread_id) = command.channel_id else {
            return respond_ephemeral(&responder, "Could not identify market.").await;
        };
        let side_label = side_name(side).to_ascii_uppercase();
        let mut input = InteractionTextInput::short("contracts", "Contracts to buy (numbers only)");
        input.label = "How many contracts?".to_owned();
        input.placeholder = Some("Enter a positive whole number".to_owned());
        input.min_length = Some(1);
        input.max_length = Some(6);
        responder
            .show_modal(InteractionModal {
                custom_id: format!("{BUY_MODAL_PREFIX}{thread_id}:{}", side_name(side)),
                title: format!("Buy {side_label} contracts"),
                inputs: vec![input],
            })
            .await
            .map_err(|error| error.to_string())
    }

    async fn buy_submit(
        &self,
        thread_id: i64,
        command: CommandContext,
        side: ContractSide,
        raw_contracts: &str,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let contracts = match raw_contracts.trim().parse::<i64>() {
            Ok(value) => value,
            Err(_) => return respond_ephemeral(&responder, "Invalid number of contracts.").await,
        };
        if contracts <= 0 {
            return respond_ephemeral(&responder, "Contracts must be positive.").await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        if command.channel_id != Some(thread_id) {
            return followup(
                &responder,
                InteractionResponse::message("This market form is no longer valid.").ephemeral(),
            )
            .await;
        }
        if !self.require_gamba_after_defer(&command, &responder).await? {
            return Ok(());
        }
        let Some(market) = self.market_for_component(Some(thread_id)).await? else {
            return followup(
                &responder,
                InteractionResponse::message("Could not identify market.").ephemeral(),
            )
            .await;
        };
        if market.status != "open" {
            return followup(
                &responder,
                InteractionResponse::message("Market is not open for trading.").ephemeral(),
            )
            .await;
        }
        let prediction_id = market.prediction_id;
        let user_id = command.user_id;
        let repository = self.predictions.clone();
        let maximum = self.config.max_contracts_per_trade;
        let trade = tokio::task::spawn_blocking(move || {
            repository.buy_contracts_atomic_with_max(
                prediction_id,
                user_id,
                side,
                contracts,
                maximum,
            )
        })
        .await;
        let trade = match trade {
            Ok(Ok(trade)) => trade,
            Ok(Err(error)) if prediction_trade_error_is_public(&error) => {
                return followup(
                    &responder,
                    InteractionResponse::message(format!("❌ {error}")).ephemeral(),
                )
                .await;
            }
            Ok(Err(error)) => {
                warn!(prediction_id, %error, "prediction buy failed unexpectedly");
                return followup(
                    &responder,
                    InteractionResponse::message(
                        "❌ Couldn't place your order right now — try again in a moment.",
                    )
                    .ephemeral(),
                )
                .await;
            }
            Err(error) => {
                warn!(prediction_id, %error, "prediction buy blocking task failed");
                return followup(
                    &responder,
                    InteractionResponse::message(
                        "❌ Couldn't place your order right now — try again in a moment.",
                    )
                    .ephemeral(),
                )
                .await;
            }
        };
        let average = (trade.vwap_x100 + 50) / 100;
        followup(
            &responder,
            InteractionResponse::message(format!(
                "✅ Bought **{} {}** @ avg price **{average}** — paid **{}** {JOPACOIN_EMOTE}. New balance: **{}**.",
                trade.contracts,
                side_name(side).to_ascii_uppercase(),
                trade.total,
                trade.new_balance
            ))
            .ephemeral(),
        )
        .await?;
        self.record_prediction_trade(user_id, &trade).await;
        if let Err(error) = self.refresh_open_market_message(prediction_id).await {
            warn!(prediction_id, %error, "failed to refresh prediction embed after trade");
        }
        Ok(())
    }

    async fn record_prediction_trade(&self, user_id: i64, trade: &TradeReceipt) {
        let pets = self.pets.clone();
        let guild_id = trade.guild_id;
        let trade_id = trade.trade_id;
        let trade_time = trade.trade_time;
        let result = spawn_sqlite("record prediction pet activity", move || {
            pets.record_activity(
                user_id,
                Some(guild_id),
                PetActivity::PredictionTraded,
                &format!("prediction-trade:{trade_id}"),
                trade_time,
            )
        })
        .await;
        if let Err(error) = result {
            debug!(%error, "prediction trade pet activity failed");
        }
    }

    async fn position_card(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let Some(market) = self.market_for_component(command.channel_id).await? else {
            return respond_ephemeral(&responder, "Could not identify market.").await;
        };
        let prediction_id = market.prediction_id;
        let user_id = command.user_id;
        let repository = self.predictions.clone();
        let (position, book) = spawn_sqlite("load prediction position", move || {
            Ok::<_, PredictionRepositoryError>((
                repository.get_position(prediction_id, user_id)?,
                repository.get_book(prediction_id)?,
            ))
        })
        .await?;
        let Some(position) = position else {
            return respond_ephemeral(&responder, "You don't hold any contracts in this market.")
                .await;
        };
        responder
            .respond(
                InteractionResponse::message("")
                    .ephemeral()
                    .embed(position_embed(
                        prediction_id,
                        position,
                        &book,
                        self.config.contract_value,
                    )),
            )
            .await
            .map_err(|error| error.to_string())
    }

    async fn refresh_open_market_message(&self, prediction_id: i64) -> Result<(), String> {
        let repository = self.predictions.clone();
        let market = spawn_sqlite("load prediction Discord identity", move || {
            repository.get_prediction(prediction_id)
        })
        .await?
        .ok_or_else(|| "Prediction not found.".to_owned())?;
        let (Some(thread_id), Some(message_id)) = (market.thread_id, market.embed_message_id)
        else {
            return Ok(());
        };
        let message = self.market_message(prediction_id, None, true).await?;
        self.market_discord
            .edit_prediction_market_message(thread_id, message_id, message)
            .await
    }

    async fn list(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let guild_id = command.guild_id.expect("explicit guild guard");
        let show_all = boolean_option(options, "show_all").unwrap_or(false);
        let repository = self.predictions.clone();
        let refresh_seconds = self.config.refresh_seconds;
        let (mut open, resolved, cancelled) = spawn_sqlite("list prediction markets", move || {
            let open = repository.list_open_markets_with_refresh(
                Some(guild_id),
                now_seconds(),
                refresh_seconds,
            )?;
            let (resolved, cancelled) = if show_all {
                (
                    repository.list_markets_by_status(Some(guild_id), "resolved")?,
                    repository.list_markets_by_status(Some(guild_id), "cancelled")?,
                )
            } else {
                (Vec::new(), Vec::new())
            };
            Ok::<_, PredictionRepositoryError>((open, resolved, cancelled))
        })
        .await?;
        open.sort_by_key(|market| std::cmp::Reverse(market.volume_recent));
        if open.is_empty() && !show_all {
            return followup(
                &responder,
                InteractionResponse::message(
                    "No open markets right now. Admins can create one with `/predict create <question> [initial_fair=50]`.",
                ),
            )
            .await;
        }
        let title = if show_all {
            format!(
                "📈 Markets ({} open · {} resolved · {} cancelled)",
                open.len(),
                resolved.len(),
                cancelled.len()
            )
        } else {
            format!("📈 Open markets ({})", open.len())
        };
        let mut embed = InteractionEmbed::titled(title).color(0x3498DB);
        let (resolved_quota, cancelled_quota) = if show_all {
            (resolved.len().min(5), cancelled.len().min(5))
        } else {
            (0, 0)
        };
        let open_quota = FIELD_CAP - resolved_quota - cancelled_quota;
        for summary in open.iter().take(open_quota) {
            let (name, value) = market_list_field(&summary.market, false);
            embed = embed.field(name, value, false);
        }
        for market in resolved.iter().take(resolved_quota) {
            let outcome = market
                .outcome
                .as_deref()
                .unwrap_or("?")
                .to_ascii_uppercase();
            embed = embed.field(
                format!("📈 #{}  ·  RESOLVED {outcome}", market.prediction_id),
                format!("\"{}\"", truncate_chars(&market.question, 200)),
                false,
            );
        }
        for market in cancelled.iter().take(cancelled_quota) {
            embed = embed.field(
                format!("📈 #{}  ·  CANCELLED", market.prediction_id),
                format!("\"{}\"", truncate_chars(&market.question, 200)),
                false,
            );
        }
        let mut footer = Vec::new();
        let skipped_open = open.len().saturating_sub(open_quota);
        let skipped_resolved = resolved.len().saturating_sub(resolved_quota);
        let skipped_cancelled = cancelled.len().saturating_sub(cancelled_quota);
        let mut overflow = Vec::new();
        if skipped_open > 0 {
            overflow.push(format!("+{skipped_open} open"));
        }
        if skipped_resolved > 0 {
            overflow.push(format!("+{skipped_resolved} resolved"));
        }
        if skipped_cancelled > 0 {
            overflow.push(format!("+{skipped_cancelled} cancelled"));
        }
        if !overflow.is_empty() {
            footer.push(format!("more not shown: {}", overflow.join(", ")));
        }
        footer.push("/predict view <id> for the full ladder".to_owned());
        footer.push("/predict help for how it works".to_owned());
        followup(
            &responder,
            InteractionResponse::message("").embed(embed.footer(footer.join("  ·  "))),
        )
        .await
    }

    async fn view(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        let prediction_id = integer_option(options, "prediction_id")
            .ok_or_else(|| "view requires prediction_id".to_owned())?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        if self
            .owned_market(command.guild_id, prediction_id, &responder)
            .await?
            .is_none()
        {
            return Ok(());
        }
        let message = self
            .market_message(prediction_id, Some(command.user_id), false)
            .await?;
        followup(&responder, message.response).await
    }

    async fn mine(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.predictions.clone();
        let user_id = command.user_id;
        let guild_id = command.guild_id;
        let positions = spawn_sqlite("load open prediction positions", move || {
            repository.get_user_open_position_summaries(user_id, guild_id)
        })
        .await?;
        if positions.is_empty() {
            return followup(
                &responder,
                InteractionResponse::message("You don't hold any open positions."),
            )
            .await;
        }
        let mut rows = vec![
            format!(
                "{:<28}{:<5}{:>5}{:>7}{:>6}{:>7}",
                "Market", "Side", "Qty", "Avg%", "Mark", "uPnL"
            ),
            "-".repeat(58),
        ];
        let mut total_cost = 0_i64;
        let mut total_mark = 0_i64;
        for summary in positions {
            append_position_rows(
                &mut rows,
                &summary,
                self.config.contract_value,
                &mut total_cost,
                &mut total_mark,
            );
        }
        let total_pnl = total_mark - total_cost;
        let embed = InteractionEmbed::titled("📈 Your open positions")
            .description(format!("```\n{}\n```", rows.join("\n")))
            .color(0x3498DB)
            .footer(format!(
                "Total cost: {total_cost} | mark {total_mark} | uPnL {total_pnl:+}"
            ));
        // Python omits `ephemeral=True` on this followup after an ephemeral
        // defer. Preserve that call-level visibility contract.
        followup(&responder, InteractionResponse::message("").embed(embed)).await
    }

    async fn refresh_status(
        &self,
        command: CommandContext,
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can view refresh status.").await;
        }
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let guild_id = command.guild_id.expect("explicit guild guard");
        let repository = self.predictions.clone();
        let refresh_seconds = self.config.refresh_seconds;
        let markets = spawn_sqlite("load prediction refresh status", move || {
            repository.list_open_markets_with_refresh(
                Some(guild_id),
                now_seconds(),
                refresh_seconds,
            )
        })
        .await?;
        if markets.is_empty() {
            return followup(
                &responder,
                InteractionResponse::message("No open markets in this guild."),
            )
            .await;
        }
        let now = now_seconds();
        let mut rows = vec![
            format!(
                "{:>4}  {:<32}  {:>14}  {:>16}",
                "PID", "Question", "Last refresh", "Next due"
            ),
            "-".repeat(72),
        ];
        for summary in markets {
            let market = summary.market;
            let last = market.last_refresh_at.unwrap_or_default();
            let (last_text, next_text) = if last <= 0 {
                ("never".to_owned(), "now (overdue)".to_owned())
            } else {
                let delta = last.saturating_add(refresh_seconds) - now;
                (
                    format!("{} ago", format_duration_short(now - last)),
                    if delta <= 0 {
                        format!("overdue {}", format_duration_short(-delta))
                    } else {
                        format!("in {}", format_duration_short(delta))
                    },
                )
            };
            rows.push(format!(
                "{:>4}  {:<32}  {:>14}  {:>16}",
                market.prediction_id,
                truncate_chars(&market.question, 30),
                last_text,
                next_text
            ));
        }
        let embed = InteractionEmbed::titled("📈 Refresh status")
            .description(format!("```\n{}\n```", rows.join("\n")))
            .color(0x3498DB)
            .footer(format!(
                "Refresh cadence: {}",
                format_duration_short(refresh_seconds)
            ));
        followup(&responder, InteractionResponse::message("").embed(embed)).await
    }

    async fn set_fair(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can override the fair price.").await;
        }
        let prediction_id = integer_option(options, "prediction_id")
            .ok_or_else(|| "set_fair requires prediction_id".to_owned())?;
        let new_price = integer_option(options, "new_price")
            .ok_or_else(|| "set_fair requires new_price".to_owned())?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some(market) = self
            .owned_market(command.guild_id, prediction_id, &responder)
            .await?
        else {
            return Ok(());
        };
        if !(self.config.price_low..=self.config.price_high).contains(&new_price) {
            return followup(
                &responder,
                InteractionResponse::message(format!(
                    "❌ new_price must be in [{}, {}].",
                    self.config.price_low, self.config.price_high
                )),
            )
            .await;
        }
        if market.status != "open" {
            return followup(
                &responder,
                InteractionResponse::message(format!(
                    "❌ Cannot set fair on a {} market.",
                    market.status
                )),
            )
            .await;
        }
        let repository = self.predictions.clone();
        let config = self.config.clone();
        let old_price = market.current_price.unwrap_or(config.initial_fair_default);
        spawn_sqlite("set prediction fair", move || {
            let effects = repository.active_prediction_effects(
                Some(market.guild_id),
                now_seconds(),
                config.economy_events_enabled,
            )?;
            let adjusted_spread = adjusted_spread(
                config.spread_ticks,
                effects.spread_ticks_delta,
                config.tick_size,
            );
            let levels = configured_levels(
                new_price,
                config.levels_per_side,
                config.size_per_level,
                config.spread_ticks,
                &config.outer_level_sizes,
                config.tick_size,
                effects.spread_ticks_delta,
            );
            repository.apply_refresh(
                prediction_id,
                new_price,
                &levels,
                now_seconds(),
                "set_fair",
                adjusted_spread.saturating_mul(config.tick_size),
            )
        })
        .await?;
        let announce = format!(
            "📈 Market #{prediction_id} fair manually set by <@{}>: YES {old_price}% → {new_price}%",
            command.user_id
        );
        followup(
            &responder,
            InteractionResponse::message(&announce)
                .with_user_mentions(vec![u64::try_from(command.user_id).unwrap_or_default()]),
        )
        .await?;
        if let Err(error) = self.refresh_open_market_message(prediction_id).await {
            warn!(prediction_id, %error, "set-fair market embed refresh failed");
        }
        if let Some(thread_id) = market.thread_id {
            let send = DiscordMessage::mentioning(
                InteractionResponse::message(announce),
                BTreeSet::from([u64::try_from(command.user_id).unwrap_or_default()]),
            );
            if let Err(error) = self
                .market_discord
                .send_prediction_thread_message(thread_id, send)
                .await
            {
                warn!(prediction_id, %error, "set-fair thread announcement failed");
            }
        }
        Ok(())
    }

    async fn force_refresh(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can force a market refresh.").await;
        }
        let prediction_id = integer_option(options, "prediction_id")
            .ok_or_else(|| "force_refresh requires prediction_id".to_owned())?;
        responder
            .defer(true)
            .await
            .map_err(|error| error.to_string())?;
        let Some(market) = self
            .owned_market(command.guild_id, prediction_id, &responder)
            .await?
        else {
            return Ok(());
        };
        if market.status != "open" {
            return followup(
                &responder,
                InteractionResponse::message("Skipped (reason: not open)."),
            )
            .await;
        }
        let repository = self.predictions.clone();
        let config = self.config.clone();
        let market_for_refresh = market.clone();
        let refresh = spawn_sqlite("force prediction refresh", move || {
            refresh_market_now(&repository, &config, &market_for_refresh)
        })
        .await?;
        let announce = format!(
            "🔧 Market #{prediction_id} manually refreshed by <@{}>: YES {}% → {}% ({} trade{} since last refresh)",
            command.user_id,
            refresh.old_price,
            refresh.new_price,
            refresh.trade_count,
            if refresh.trade_count == 1 { "" } else { "s" }
        );
        followup(
            &responder,
            InteractionResponse::message(&announce)
                .with_user_mentions(vec![u64::try_from(command.user_id).unwrap_or_default()]),
        )
        .await?;
        if let Err(error) = self.refresh_open_market_message(prediction_id).await {
            warn!(prediction_id, %error, "force-refresh market embed update failed");
        }
        if let Some(thread_id) = market.thread_id {
            let send = DiscordMessage::mentioning(
                InteractionResponse::message(announce),
                BTreeSet::from([u64::try_from(command.user_id).unwrap_or_default()]),
            );
            if let Err(error) = self
                .market_discord
                .send_prediction_thread_message(thread_id, send)
                .await
            {
                warn!(prediction_id, %error, "force-refresh thread announcement failed");
            }
        }
        Ok(())
    }

    async fn cancel(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can cancel markets.").await;
        }
        let prediction_id = integer_option(options, "prediction_id")
            .ok_or_else(|| "cancel requires prediction_id".to_owned())?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some(market) = self
            .owned_market(command.guild_id, prediction_id, &responder)
            .await?
        else {
            return Ok(());
        };
        let repository = self.predictions.clone();
        let cancellation = spawn_sqlite("cancel prediction market", move || {
            repository.cancel_orderbook_detailed(prediction_id)
        })
        .await;
        let cancellation = match cancellation {
            Ok(value) => value,
            Err(error) => {
                return followup(
                    &responder,
                    InteractionResponse::message(format!("❌ {error}")),
                )
                .await;
            }
        };
        let announce = cancellation_announcement(&cancellation, command.user_id);
        followup(
            &responder,
            InteractionResponse::message(&announce)
                .with_user_mentions(vec![u64::try_from(command.user_id).unwrap_or_default()]),
        )
        .await?;
        if let (Some(thread_id), Some(message_id)) = (market.thread_id, market.embed_message_id) {
            match self.market_message(prediction_id, None, false).await {
                Ok(message) => {
                    if let Err(error) = self
                        .command_discord
                        .edit_prediction_command_message(thread_id, message_id, message)
                        .await
                    {
                        warn!(prediction_id, %error, "cancelled market terminal edit failed");
                    }
                }
                Err(error) => warn!(prediction_id, %error, "cancelled market render failed"),
            }
            if let Err(error) = self
                .market_discord
                .send_prediction_thread_message(
                    thread_id,
                    DiscordMessage::mentioning(
                        InteractionResponse::message(&announce),
                        BTreeSet::from([u64::try_from(command.user_id).unwrap_or_default()]),
                    ),
                )
                .await
            {
                warn!(prediction_id, %error, "cancel thread announcement failed");
            }
            if let Err(error) = self
                .command_discord
                .archive_prediction_thread(thread_id)
                .await
            {
                warn!(prediction_id, %error, "cancelled market archive failed");
            }
        }
        self.announce_to_gamba(
            market.guild_id,
            DiscordMessage::mentioning(
                InteractionResponse::message(announce),
                BTreeSet::from([u64::try_from(command.user_id).unwrap_or_default()]),
            ),
        )
        .await;
        Ok(())
    }

    async fn rollback(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can roll back markets.").await;
        }
        let prediction_id = integer_option(options, "prediction_id")
            .ok_or_else(|| "rollback requires prediction_id".to_owned())?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let repository = self.predictions.clone();
        let resolutions = self.resolutions.clone();
        let config = self.config.clone();
        let guild_id = command.guild_id.unwrap_or_default();
        let actor_id = command.user_id;
        let rollback = spawn_sqlite("rollback prediction resolution", move || {
            let market = repository
                .get_prediction(prediction_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| PredictionRepositoryError::PredictionNotFound.to_string())?;
            let current_price = market
                .current_price
                .ok_or_else(|| PredictionRepositoryError::PredictionNotFound.to_string())?;
            let effects = repository
                .active_prediction_effects(
                    Some(market.guild_id),
                    now_seconds(),
                    config.economy_events_enabled,
                )
                .map_err(|error| error.to_string())?;
            let levels = configured_levels(
                current_price,
                config.levels_per_side,
                config.size_per_level,
                config.spread_ticks,
                &config.outer_level_sizes,
                config.tick_size,
                effects.spread_ticks_delta,
            );
            resolutions
                .rollback_orderbook(prediction_id, guild_id, &levels, Some(actor_id))
                .map_err(|error| error.to_string())
        })
        .await;
        let rollback = match rollback {
            Ok(value) => value,
            Err(error) => {
                return followup(
                    &responder,
                    InteractionResponse::message(format!("❌ {error}")),
                )
                .await;
            }
        };
        let announce = format!(
            "↩️ Market #{prediction_id} resolution rolled back by <@{}>. Previous outcome: **{}**. Reversed {} {JOPACOIN_EMOTE} across {} holder(s). Trading is open again.",
            command.user_id,
            side_name(rollback.previous_outcome).to_ascii_uppercase(),
            rollback.total_reversed,
            rollback.affected_players
        );
        followup(
            &responder,
            InteractionResponse::message(&announce)
                .with_user_mentions(vec![u64::try_from(command.user_id).unwrap_or_default()]),
        )
        .await?;
        let repository = self.predictions.clone();
        let market = spawn_sqlite("load rolled-back prediction", move || {
            repository.get_prediction(prediction_id)
        })
        .await?;
        if let Some(market) = market {
            if let Some(thread_id) = market.thread_id {
                if let Err(error) = self
                    .command_discord
                    .reopen_prediction_thread(thread_id)
                    .await
                {
                    warn!(prediction_id, %error, "rollback thread reopen failed");
                }
                if let Some(message_id) = market.embed_message_id {
                    match self.market_message(prediction_id, None, true).await {
                        Ok(message) => {
                            if let Err(error) = self
                                .command_discord
                                .edit_prediction_command_message(thread_id, message_id, message)
                                .await
                            {
                                warn!(prediction_id, %error, "rollback market edit failed");
                            }
                        }
                        Err(error) => warn!(prediction_id, %error, "rollback render failed"),
                    }
                }
                if let Err(error) = self
                    .market_discord
                    .send_prediction_thread_message(
                        thread_id,
                        DiscordMessage::mentioning(
                            InteractionResponse::message(&announce),
                            BTreeSet::from([u64::try_from(command.user_id).unwrap_or_default()]),
                        ),
                    )
                    .await
                {
                    warn!(prediction_id, %error, "rollback thread announcement failed");
                }
            }
            self.announce_to_gamba(
                market.guild_id,
                DiscordMessage::mentioning(
                    InteractionResponse::message(announce),
                    BTreeSet::from([u64::try_from(command.user_id).unwrap_or_default()]),
                ),
            )
            .await;
        }
        Ok(())
    }

    async fn resolve(
        &self,
        command: CommandContext,
        options: &[InteractionOption],
        responder: Arc<dyn InteractionResponder>,
    ) -> Result<(), String> {
        if !self.is_admin(&command) {
            return respond_ephemeral(&responder, "Only admins can resolve markets.").await;
        }
        let prediction_id = integer_option(options, "prediction_id")
            .ok_or_else(|| "resolve requires prediction_id".to_owned())?;
        let outcome = parse_side(
            string_option(options, "outcome")
                .ok_or_else(|| "resolve requires outcome".to_owned())?,
        )?;
        responder
            .defer(false)
            .await
            .map_err(|error| error.to_string())?;
        let Some(market) = self
            .owned_market(command.guild_id, prediction_id, &responder)
            .await?
        else {
            return Ok(());
        };
        let display_question = self
            .command_discord
            .prediction_display_text(market.guild_id, &market.question)
            .await
            .unwrap_or_else(|_| market.question.clone());
        let taxable = self.vanity_tax.taxable_ids(market.guild_id);
        let repository = self.predictions.clone();
        let resolutions = self.resolutions.clone();
        let config = self.config.clone();
        let resolved_by = command.user_id;
        let market_guild_id = market.guild_id;
        let resolution = spawn_sqlite("resolve prediction market", move || {
            let effects = repository
                .active_prediction_effects(
                    Some(market_guild_id),
                    now_seconds(),
                    config.economy_events_enabled,
                )
                .map_err(|error| error.to_string())?;
            resolutions
                .settle_orderbook(
                    prediction_id,
                    outcome,
                    Some(resolved_by),
                    &SettlementAdjustments {
                        contract_value: config.contract_value,
                        payout_multiplier_bps: effects.payout_multiplier_bps,
                        bankruptcy_credit_bps: Some(config.bankruptcy_credit_bps),
                        vanity_tax_bps: config.vanity_tax_bps,
                        vanity_taxable_ids: taxable,
                    },
                )
                .map_err(|error| error.to_string())
        })
        .await;
        let resolution = match resolution {
            Ok(value) => value,
            Err(error) => {
                return followup(
                    &responder,
                    InteractionResponse::message(format!("❌ {error}")),
                )
                .await;
            }
        };
        let chunks = resolution_chunks(&resolution, &display_question);
        let chart = self.prediction_chart(prediction_id).await.ok();
        for (index, chunk) in chunks.iter().enumerate() {
            self.followup_with_optional_chart(
                &responder,
                chunk,
                (index == 0).then(|| chart.clone()).flatten(),
                resolution_mention_ids(&resolution),
            )
            .await?;
        }

        // Python evaluates this only after the public settlement chunks have
        // succeeded and before it rewrites/archives the durable market thread.
        // Neon remains best-effort: media/provider/Discord failures never turn
        // a committed settlement into an interaction failure.
        if let Some(channel_id) = command.channel_id
            && let Some(participant) = biggest_prediction_winner(&resolution)
        {
            let event = PredictionBigWin {
                discord_id: participant.discord_id,
                guild_id: resolution.guild_id,
                channel_id,
                payout: participant.payout,
            };
            if let Err(error) = self.neon.on_big_win(event).await {
                debug!(prediction_id, %error, "prediction big-win Neon failed");
            }
        }

        let repository = self.predictions.clone();
        let settled_market = spawn_sqlite("load settled prediction", move || {
            repository.get_prediction(prediction_id)
        })
        .await?
        .unwrap_or_else(|| market.clone());
        if let (Some(thread_id), Some(message_id)) =
            (settled_market.thread_id, settled_market.embed_message_id)
        {
            match self.market_message(prediction_id, None, false).await {
                Ok(message) => {
                    if let Err(error) = self
                        .command_discord
                        .edit_prediction_command_message(thread_id, message_id, message)
                        .await
                    {
                        warn!(prediction_id, %error, "resolved market terminal edit failed");
                    }
                }
                Err(error) => warn!(prediction_id, %error, "resolved market render failed"),
            }
            for (index, chunk) in chunks.iter().enumerate() {
                let attachment = (index == 0).then(|| chart.clone()).flatten();
                self.send_thread_with_optional_chart(
                    thread_id,
                    chunk,
                    attachment,
                    resolution_mention_ids(&resolution),
                )
                .await;
            }
            if let Err(error) = self
                .command_discord
                .archive_prediction_thread(thread_id)
                .await
            {
                warn!(prediction_id, %error, "resolved market archive failed");
            }
        }
        for (index, chunk) in chunks.iter().enumerate() {
            let attachment = (index == 0).then(|| chart.clone()).flatten();
            self.announce_to_gamba(
                settled_market.guild_id,
                message_with_optional_chart(chunk, attachment, resolution_mention_ids(&resolution)),
            )
            .await;
        }
        Ok(())
    }

    async fn prediction_chart(&self, prediction_id: i64) -> Result<InteractionAttachment, String> {
        let (snapshot, history) = self
            .load_market_display(prediction_id, None)
            .await?
            .ok_or_else(|| "Prediction not found.".to_owned())?;
        let title = self
            .command_discord
            .prediction_display_text(snapshot.market.guild_id, &snapshot.market.question)
            .await
            .unwrap_or(snapshot.market.question);
        Ok(InteractionAttachment::bytes(
            format!("predict_{prediction_id}.png"),
            draw_prediction_market_chart(
                prediction_id,
                Some(&title),
                &history,
                snapshot.market.created_at,
                now_seconds(),
            )
            .into_inner(),
        ))
    }

    async fn followup_with_optional_chart(
        &self,
        responder: &Arc<dyn InteractionResponder>,
        content: &str,
        chart: Option<InteractionAttachment>,
        mentions: BTreeSet<u64>,
    ) -> Result<(), String> {
        let mut response = InteractionResponse::message(content)
            .with_user_mentions(mentions.iter().copied().collect());
        if let Some(chart) = chart {
            response = response.attachment(chart);
            if responder.followup(response).await.is_ok() {
                return Ok(());
            }
            warn!("prediction resolution attachment followup failed; retrying text-only");
        }
        followup(
            responder,
            InteractionResponse::message(content)
                .with_user_mentions(mentions.into_iter().collect()),
        )
        .await
    }

    async fn send_thread_with_optional_chart(
        &self,
        thread_id: i64,
        content: &str,
        chart: Option<InteractionAttachment>,
        mentions: BTreeSet<u64>,
    ) {
        let with_chart = message_with_optional_chart(content, chart.clone(), mentions.clone());
        if self
            .market_discord
            .send_prediction_thread_message(thread_id, with_chart)
            .await
            .is_ok()
        {
            return;
        }
        if chart.is_some()
            && let Err(error) = self
                .market_discord
                .send_prediction_thread_message(
                    thread_id,
                    message_with_optional_chart(content, None, mentions),
                )
                .await
        {
            warn!(%error, "prediction resolution thread send failed");
        }
    }

    async fn announce_to_gamba(&self, guild_id: i64, message: DiscordMessage) {
        let destination =
            self.gamba_guilds
                .live_gamba_destinations()
                .ok()
                .and_then(|destinations| {
                    destinations
                        .into_iter()
                        .find(|destination| destination.guild_id == guild_id)
                });
        let Some(destination) = destination else {
            return;
        };
        let Ok(channel_id) = u64::try_from(destination.channel_id) else {
            return;
        };
        let retry = (!message.response.attachments.is_empty()).then(|| {
            let mut message = message.clone();
            message.response.attachments.clear();
            message
        });
        if let Err(error) = self.discord.send_message(channel_id, message).await {
            if let Some(retry) = retry {
                if let Err(retry_error) = self.discord.send_message(channel_id, retry).await {
                    warn!(%retry_error, "prediction gamba attachment retry failed");
                }
            } else {
                debug!(%error, "prediction gamba announcement failed");
            }
        }
    }
}

fn prediction_subcommands() -> Vec<CommandOptionSpec> {
    let mut outcome =
        CommandOptionSpec::new("outcome", "YES or NO", CommandOptionKind::String).required(true);
    outcome.choices = vec![
        CommandOptionChoice::String {
            name: "YES".to_owned(),
            value: "yes".to_owned(),
        },
        CommandOptionChoice::String {
            name: "NO".to_owned(),
            value: "no".to_owned(),
        },
    ];
    vec![
        subcommand(
            "create",
            "Create a new prediction market (admin)",
            vec![
                required_option(
                    "question",
                    "The question to predict on",
                    CommandOptionKind::String,
                ),
                CommandOptionSpec::new(
                    "initial_fair",
                    "Starting price (= % implied probability)",
                    CommandOptionKind::Integer,
                ),
            ],
        ),
        subcommand(
            "resolve",
            "Resolve a market YES or NO (admin)",
            vec![required_integer("prediction_id", "Market ID"), outcome],
        ),
        subcommand(
            "rollback",
            "Roll back a resolution and reopen the market (admin)",
            vec![required_integer(
                "prediction_id",
                "Resolved market ID to reopen",
            )],
        ),
        subcommand(
            "set_fair",
            "Manually override a market's fair price (admin)",
            vec![
                required_integer("prediction_id", "Market ID"),
                required_integer("new_price", "New fair price"),
            ],
        ),
        subcommand(
            "cancel",
            "Cancel a market and refund cost basis (admin)",
            vec![required_integer("prediction_id", "Market ID to cancel")],
        ),
        subcommand(
            "list",
            "List open markets (or all with show_all)",
            vec![CommandOptionSpec::new(
                "show_all",
                "Include resolved/cancelled markets",
                CommandOptionKind::Boolean,
            )],
        ),
        subcommand(
            "view",
            "Show a market's embed (price, ladder, recent trades)",
            vec![required_integer("prediction_id", "Market ID")],
        ),
        subcommand(
            "help",
            "Explainer: how prediction markets work in this bot",
            vec![],
        ),
        subcommand("mine", "Your open positions across markets", vec![]),
        subcommand(
            "force_refresh",
            "Manually trigger a market refresh (admin)",
            vec![required_integer("prediction_id", "Market ID to refresh")],
        ),
        subcommand(
            "refresh_status",
            "Show refresh state for all open markets in this guild (admin)",
            vec![],
        ),
    ]
}

fn subcommand(name: &str, description: &str, options: Vec<CommandOptionSpec>) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, CommandOptionKind::Subcommand).options(options)
}

fn required_option(name: &str, description: &str, kind: CommandOptionKind) -> CommandOptionSpec {
    CommandOptionSpec::new(name, description, kind).required(true)
}

fn required_integer(name: &str, description: &str) -> CommandOptionSpec {
    required_option(name, description, CommandOptionKind::Integer)
}

fn leaf_command(options: &[InteractionOption]) -> Result<(String, &[InteractionOption]), String> {
    let option = options
        .iter()
        .find(|option| matches!(option.value, InteractionValue::Subcommand(_)))
        .ok_or_else(|| "predict command is missing a subcommand".to_owned())?;
    let InteractionValue::Subcommand(children) = &option.value else {
        unreachable!("filtered to subcommand")
    };
    Ok((option.name.clone(), children))
}

fn integer_option(options: &[InteractionOption], name: &str) -> Option<i64> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::Integer(value) => Some(*value),
                _ => None,
            })
    })
}

fn string_option<'a>(options: &'a [InteractionOption], name: &str) -> Option<&'a str> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::String(value) => Some(value.as_str()),
                _ => None,
            })
    })
}

fn boolean_option(options: &[InteractionOption], name: &str) -> Option<bool> {
    options.iter().find_map(|option| {
        (option.name == name)
            .then_some(&option.value)
            .and_then(|value| match value {
                InteractionValue::Boolean(value) => Some(*value),
                _ => None,
            })
    })
}

fn rate_to_bps(rate: f64) -> i64 {
    if rate.is_finite() {
        (rate.clamp(0.0, 1.0) * BASIS_POINTS as f64).round() as i64
    } else {
        0
    }
}

async fn spawn_sqlite<T, E, F>(operation: &'static str, function: F) -> Result<T, String>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    crate::ids::blocking(operation, move || {
        function().map_err(|error| error.to_string())
    })
    .await
}

async fn respond_ephemeral(
    responder: &Arc<dyn InteractionResponder>,
    content: impl Into<String>,
) -> Result<(), String> {
    responder
        .respond(
            InteractionResponse::message(content)
                .ephemeral()
                .without_mentions(),
        )
        .await
        .map_err(|error| error.to_string())
}

async fn followup(
    responder: &Arc<dyn InteractionResponder>,
    response: InteractionResponse,
) -> Result<(), String> {
    responder
        .followup(response)
        .await
        .map_err(|error| error.to_string())
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().try_into().unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn parse_side(value: &str) -> Result<ContractSide, String> {
    match value {
        "yes" => Ok(ContractSide::Yes),
        "no" => Ok(ContractSide::No),
        _ => Err("side must be 'yes' or 'no'".to_owned()),
    }
}

fn prediction_trade_error_is_public(error: &PredictionRepositoryError) -> bool {
    !matches!(
        error,
        PredictionRepositoryError::Sqlite(_)
            | PredictionRepositoryError::ClockBeforeUnixEpoch
            | PredictionRepositoryError::ArithmeticOverflow
            | PredictionRepositoryError::InvalidLevel
            | PredictionRepositoryError::InvalidStatus(_)
    )
}

const fn side_name(side: ContractSide) -> &'static str {
    match side {
        ContractSide::Yes => "yes",
        ContractSide::No => "no",
    }
}

fn parse_buy_modal_id(custom_id: &str) -> Result<(i64, ContractSide), String> {
    let suffix = custom_id
        .strip_prefix(BUY_MODAL_PREFIX)
        .ok_or_else(|| "invalid prediction buy modal".to_owned())?;
    let (prediction_id, side) = suffix
        .split_once(':')
        .ok_or_else(|| "invalid prediction buy modal".to_owned())?;
    let prediction_id = prediction_id
        .parse()
        .map_err(|_| "invalid prediction ID in buy modal".to_owned())?;
    Ok((prediction_id, parse_side(side)?))
}

fn persistent_market_buttons() -> InteractionActionRow {
    InteractionActionRow::buttons(vec![
        InteractionButton::new(BUY_YES_ID, "BUY YES")
            .emoji("🟢")
            .style(InteractionButtonStyle::Success),
        InteractionButton::new(BUY_NO_ID, "BUY NO")
            .emoji("🔴")
            .style(InteractionButtonStyle::Danger),
        InteractionButton::new(MY_POSITION_ID, "MY POSITION")
            .emoji("📊")
            .style(InteractionButtonStyle::Secondary),
    ])
}

fn resolved_embed(market: &Market) -> InteractionEmbed {
    let outcome = market
        .outcome
        .as_deref()
        .unwrap_or("?")
        .to_ascii_uppercase();
    InteractionEmbed::titled(format!("📈 Market #{}", market.prediction_id))
        .description(format!("\"{}\"", market.question))
        .color(if outcome == "YES" { 0x2ECC71 } else { 0xE74C3C })
        .field("Outcome", format!("**{outcome}**"), false)
}

fn cancelled_embed(market: &Market) -> InteractionEmbed {
    InteractionEmbed::titled(format!("📈 Market #{}", market.prediction_id))
        .description(format!("\"{}\"", market.question))
        .color(0x95A5A6)
        .field("Status", "**CANCELLED** — cost basis refunded.", false)
}

fn ladder_body(book: &Book) -> String {
    let asks_all = &book.yes_asks;
    let bids_all = &book.yes_bids;
    let hidden_asks = asks_all.len().saturating_sub(LADDER_MAX_ROWS_PER_SIDE);
    let hidden_bids = bids_all.len().saturating_sub(LADDER_MAX_ROWS_PER_SIDE);
    let asks = &asks_all[..asks_all.len().min(LADDER_MAX_ROWS_PER_SIDE)];
    let bids = &bids_all[..bids_all.len().min(LADDER_MAX_ROWS_PER_SIDE)];
    let green = "\u{1b}[0;32m";
    let red = "\u{1b}[0;31m";
    let reset = "\u{1b}[0;0m";
    let row = |price: i64, size: i64| {
        let cells = usize::try_from((size / 10).clamp(0, 10)).unwrap_or_default();
        format!("  {price:>3}  {:<10}  {size:>3}", "█".repeat(cells))
    };
    let mut lines = vec![format!("{green}🟢 Buy YES{reset}")];
    if asks.is_empty() {
        lines.push("  (none — refreshes daily)".to_owned());
    } else {
        if hidden_asks > 0 {
            lines.push(format!(
                "  (+{hidden_asks} deeper to {})",
                asks_all.last().map_or(0, |level| level.0)
            ));
        }
        lines.extend(
            asks.iter()
                .rev()
                .map(|(price, size)| format!("{green}{}{reset}", row(*price, *size))),
        );
    }
    lines.push(String::new());
    let cheapest_yes = asks.first().map(|level| level.0);
    let cheapest_no = bids.first().map(|level| 100 - level.0);
    if cheapest_yes.is_none() && cheapest_no.is_none() && book.current_price.is_none() {
        lines.push("  ── ? ──".to_owned());
    } else {
        let yes = cheapest_yes.map_or_else(|| "YES —".to_owned(), |value| format!("YES {value}%"));
        let no = cheapest_no.map_or_else(|| "NO —".to_owned(), |value| format!("NO {value}%"));
        lines.push(format!("  ── {yes}  ·  {no} ──"));
    }
    lines.push(String::new());
    lines.push(format!("{red}🔴 Buy NO{reset}"));
    if bids.is_empty() {
        lines.push("  (none — refreshes daily)".to_owned());
    } else {
        lines.extend(
            bids.iter()
                .map(|(price, size)| format!("{red}{}{reset}", row(100 - *price, *size))),
        );
        if hidden_bids > 0 {
            lines.push(format!(
                "  (+{hidden_bids} deeper to {})",
                bids_all.last().map_or(0, |level| 100 - level.0)
            ));
        }
    }
    format!("```ansi\n{}\n```", lines.join("\n"))
}

fn recent_trade_text(
    trades: &[Trade],
    guild_id: i64,
    discord: &dyn PredictionCommandDiscordPort,
) -> String {
    if trades.is_empty() {
        return "_no trades yet_".to_owned();
    }
    trades
        .iter()
        .map(|trade| {
            let name = discord.prediction_display_name(guild_id, trade.discord_id);
            let verb = if trade.action.starts_with("buy") {
                "bought"
            } else {
                "sold"
            };
            let side = if trade.action.ends_with("yes") {
                "YES"
            } else {
                "NO"
            };
            format!(
                "  {name} {verb} {} {side} @ {}",
                trade.contracts,
                (trade.vwap_x100 + 50) / 100
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn position_mark(book: &Book, side: ContractSide) -> Option<i64> {
    match side {
        ContractSide::Yes => book.yes_bids.first().map(|level| level.0),
        ContractSide::No => book.yes_asks.first().map(|level| 100 - level.0),
    }
}

fn average_price(cost_basis: i64, contracts: i64, contract_value: i64) -> f64 {
    if contracts <= 0 || contract_value == 0 {
        0.0
    } else {
        cost_basis as f64 * 100.0 / (contracts as f64 * contract_value as f64)
    }
}

fn unrealized_pnl(mark: i64, average: f64, contracts: i64, contract_value: i64) -> i64 {
    ((mark as f64 - average) * contracts as f64 * contract_value as f64 / 100.0) as i64
}

fn position_card_text(position: Position, book: &Book, contract_value: i64) -> Option<String> {
    let mut parts = Vec::new();
    if position.yes_contracts > 0 {
        let average = average_price(
            position.yes_cost_basis_total,
            position.yes_contracts,
            contract_value,
        );
        let mark = position_mark(book, ContractSide::Yes);
        let pnl = mark.map_or(0, |mark| {
            unrealized_pnl(mark, average, position.yes_contracts, contract_value)
        });
        parts.push(format!(
            "YES {} @ avg {average:.1} (mark {}, uPnL {pnl:+})",
            position.yes_contracts,
            mark.map_or_else(|| "-".to_owned(), |value| value.to_string())
        ));
    }
    if position.no_contracts > 0 {
        let average = average_price(
            position.no_cost_basis_total,
            position.no_contracts,
            contract_value,
        );
        let mark = position_mark(book, ContractSide::No);
        let pnl = mark.map_or(0, |mark| {
            unrealized_pnl(mark, average, position.no_contracts, contract_value)
        });
        parts.push(format!(
            "NO  {} @ avg {average:.1} (mark {}, uPnL {pnl:+})",
            position.no_contracts,
            mark.map_or_else(|| "-".to_owned(), |value| value.to_string())
        ));
    }
    (!parts.is_empty()).then(|| format!("Your position: {}", parts.join(" | ")))
}

fn position_embed(
    prediction_id: i64,
    position: Position,
    book: &Book,
    contract_value: i64,
) -> InteractionEmbed {
    let mut embed = InteractionEmbed::titled(format!("Your position in market #{prediction_id}"))
        .color(0x3498DB);
    if position.yes_contracts > 0 {
        let average = average_price(
            position.yes_cost_basis_total,
            position.yes_contracts,
            contract_value,
        );
        let mark = position_mark(book, ContractSide::Yes);
        let pnl = mark.map_or(0, |mark| {
            unrealized_pnl(mark, average, position.yes_contracts, contract_value)
        });
        embed = embed.field(
            "YES",
            format!(
                "contracts: **{}**\navg cost: {average:.1}\nmark: {}\nuPnL: **{pnl:+}**",
                position.yes_contracts,
                mark.map_or_else(|| "-".to_owned(), |value| value.to_string())
            ),
            true,
        );
    }
    if position.no_contracts > 0 {
        let average = average_price(
            position.no_cost_basis_total,
            position.no_contracts,
            contract_value,
        );
        let mark = position_mark(book, ContractSide::No);
        let pnl = mark.map_or(0, |mark| {
            unrealized_pnl(mark, average, position.no_contracts, contract_value)
        });
        embed = embed.field(
            "NO",
            format!(
                "contracts: **{}**\navg cost: {average:.1}\nmark: {}\nuPnL: **{pnl:+}**",
                position.no_contracts,
                mark.map_or_else(|| "-".to_owned(), |value| value.to_string())
            ),
            true,
        );
    }
    embed
}

fn market_list_field(market: &Market, with_delta: bool) -> (String, String) {
    let price = market
        .current_price
        .map_or_else(|| "?".to_owned(), |value| format!("{value}%"));
    let delta = if with_delta {
        match (market.current_price, market.prev_price) {
            (Some(current), Some(previous)) if current != previous => format!(
                "  ({}{} today)",
                if current > previous { "↑" } else { "↓" },
                (current - previous).abs()
            ),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let name = format!("📈 #{}  ·  YES {price}{delta}", market.prediction_id);
    let question = truncate_chars(market.question.trim(), 200);
    let mut value = if question.is_empty() {
        "—".to_owned()
    } else {
        format!("\"{question}\"")
    };
    if let Some(thread_id) = market.thread_id {
        let link = market.embed_message_id.map_or_else(
            || {
                format!(
                    "https://discord.com/channels/{}/{thread_id}",
                    market.guild_id
                )
            },
            |message_id| {
                format!(
                    "https://discord.com/channels/{}/{thread_id}/{message_id}",
                    market.guild_id
                )
            },
        );
        value.push_str(&format!("\n[Trade →]({link})"));
    }
    (name, value)
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        value.to_owned()
    } else if maximum >= 3 {
        format!("{}...", value.chars().take(maximum - 3).collect::<String>())
    } else {
        value.chars().take(maximum).collect()
    }
}

fn append_position_rows(
    rows: &mut Vec<String>,
    summary: &OpenPositionSummary,
    contract_value: i64,
    total_cost: &mut i64,
    total_mark: &mut i64,
) {
    let question = truncate_chars(&summary.question, 26);
    for (side, contracts, basis, mark) in [
        (
            "YES",
            summary.marked.position.yes_contracts,
            summary.marked.position.yes_cost_basis_total,
            summary.marked.yes_mark,
        ),
        (
            "NO",
            summary.marked.position.no_contracts,
            summary.marked.position.no_cost_basis_total,
            summary.marked.no_mark,
        ),
    ] {
        if contracts == 0 {
            continue;
        }
        let average = average_price(basis, contracts, contract_value);
        let mark_value = mark
            .unwrap_or_default()
            .saturating_mul(contracts)
            .saturating_mul(contract_value)
            / 100;
        let pnl = mark_value - basis;
        *total_cost = total_cost.saturating_add(basis);
        *total_mark = total_mark.saturating_add(mark_value);
        rows.push(format!(
            "{question:<28}{side:<5}{contracts:>5}{average:>7.1}{:>6}{pnl:>+7}",
            mark.unwrap_or_default()
        ));
    }
}

fn format_duration_short(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 86_400 {
        format!("{}d", seconds / 86_400)
    } else if seconds >= 3_600 {
        format!("{}h", seconds / 3_600)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        // Python's `format_duration_short` deliberately never renders `0s`.
        format!("{}s", seconds.max(1))
    }
}

fn adjusted_spread(base: i64, delta: i64, tick_size: i64) -> i64 {
    let maximum = (98 / tick_size.max(1)).max(1);
    base.saturating_add(delta).clamp(1, maximum)
}

fn configured_levels(
    fair: i64,
    normal_levels: usize,
    size_per_level: i64,
    spread_ticks: i64,
    outer_level_sizes: &[i64],
    tick_size: i64,
    spread_delta: i64,
) -> Vec<NewLevel> {
    if size_per_level <= 0 {
        return Vec::new();
    }
    let spread = adjusted_spread(spread_ticks, spread_delta, tick_size);
    std::iter::repeat_n(size_per_level, normal_levels)
        .chain(outer_level_sizes.iter().map(|size| (*size).max(0)))
        .enumerate()
        .flat_map(|(index, size)| {
            if size == 0 {
                return [None, None];
            }
            let index = i64::try_from(index).unwrap_or(i64::MAX);
            let offset = spread.saturating_add(index).saturating_mul(tick_size);
            let ask = fair.saturating_add(offset);
            let bid = fair.saturating_sub(offset);
            [
                (1..=99).contains(&ask).then(|| NewLevel::ask(ask, size)),
                (1..=99).contains(&bid).then(|| NewLevel::bid(bid, size)),
            ]
        })
        .flatten()
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManualRefresh {
    old_price: i64,
    new_price: i64,
    trade_count: i64,
}

fn refresh_market_now(
    repository: &PredictionRepository,
    config: &PredictionCommandConfig,
    market: &Market,
) -> Result<ManualRefresh, PredictionRepositoryError> {
    let book = repository.get_book(market.prediction_id)?;
    let since = market.last_refresh_at.unwrap_or_default();
    let observed = match (book.yes_asks.first(), book.yes_bids.first()) {
        (Some(ask), Some(bid)) => (ask.0 + bid.0) as f64 / 2.0,
        (None, Some(bid)) => {
            repository
                .last_fill_price_since(market.prediction_id, &["buy_yes", "sell_no"], since)?
                .unwrap_or(bid.0) as f64
                + config.fade_ticks as f64
        }
        (Some(ask), None) => {
            repository
                .last_fill_price_since(market.prediction_id, &["buy_no", "sell_yes"], since)?
                .unwrap_or(ask.0) as f64
                - config.fade_ticks as f64
        }
        (None, None) => {
            let lifted = repository.last_fill_price_since(
                market.prediction_id,
                &["buy_yes", "sell_no"],
                since,
            )?;
            let hit = repository.last_fill_price_since(
                market.prediction_id,
                &["buy_no", "sell_yes"],
                since,
            )?;
            match (lifted, hit) {
                (Some(ask), Some(bid)) => (ask + bid) as f64 / 2.0,
                (Some(ask), None) => (ask + config.fade_ticks) as f64,
                (None, Some(bid)) => (bid - config.fade_ticks) as f64,
                (None, None) => market.current_price.unwrap_or(config.initial_fair_default) as f64,
            }
        }
    };
    let drift = if config.drift_min >= config.drift_max {
        config.drift_min
    } else {
        fastrand::i64(config.drift_min..=config.drift_max)
    };
    let old_price = market.current_price.unwrap_or(config.initial_fair_default);
    let new_price = (python_round(observed) + drift).clamp(config.price_low, config.price_high);
    let now = now_seconds();
    let effects = repository.active_prediction_effects(
        Some(market.guild_id),
        now,
        config.economy_events_enabled,
    )?;
    let levels = configured_levels(
        new_price,
        config.refresh_levels_per_side,
        config.refresh_size_per_level,
        config.refresh_spread_ticks,
        &config.refresh_outer_level_sizes,
        config.tick_size,
        effects.spread_ticks_delta,
    );
    let minimum_offset = adjusted_spread(
        config.spread_ticks,
        effects.spread_ticks_delta,
        config.tick_size,
    )
    .saturating_mul(config.tick_size);
    repository.apply_refresh(
        market.prediction_id,
        new_price,
        &levels,
        now,
        "refresh",
        minimum_offset,
    )?;
    Ok(ManualRefresh {
        old_price,
        new_price,
        trade_count: repository.trade_count_since(market.prediction_id, since)?,
    })
}

fn python_round(value: f64) -> i64 {
    let floor = value.floor();
    let fraction = value - floor;
    if (fraction - 0.5).abs() < f64::EPSILON {
        let floor = floor as i64;
        if floor % 2 == 0 { floor } else { floor + 1 }
    } else {
        value.round() as i64
    }
}

fn cancellation_announcement(cancellation: &Cancellation, actor_id: i64) -> String {
    format!(
        "🚫 Market #{} cancelled by <@{actor_id}>. Refunded {} {JOPACOIN_EMOTE} across {} holder(s).",
        cancellation.prediction_id,
        cancellation.total_refunded,
        cancellation.refunded.len()
    )
}

fn resolution_chunks(resolution: &Resolution, question: &str) -> Vec<String> {
    let mut participants = resolution.participants.clone();
    participants.sort_by_key(|participant| std::cmp::Reverse(participant.profit));
    let up = participants
        .iter()
        .filter(|participant| participant.profit > 0)
        .count();
    let down = participants
        .iter()
        .filter(|participant| participant.profit < 0)
        .count();
    let mut lines = vec![format!(
        "📈 Market #{} resolved **{}** — {up} up, {down} down.",
        resolution.prediction_id,
        side_name(resolution.outcome).to_ascii_uppercase()
    )];
    if !question.is_empty() {
        lines.push(format!("\"{question}\""));
    }
    if participants.is_empty() {
        lines.push("No participants.".to_owned());
    } else {
        lines.extend(participants.iter().map(|participant| {
            format!(
                "<@{}> spent {} ({} yes / {} no) → won {} JC (net {:+})",
                participant.discord_id,
                participant.cost_basis,
                participant.yes_contracts,
                participant.no_contracts,
                participant.payout,
                participant.profit
            )
        }));
    }
    let bankruptcy: i64 = participants
        .iter()
        .map(|participant| participant.bankruptcy_penalty)
        .sum();
    let vanity: i64 = participants
        .iter()
        .map(|participant| participant.vanity_tax)
        .sum();
    if bankruptcy > 0 {
        lines.push(format!("({bankruptcy} JC withheld from bankrupt winners.)"));
    }
    if vanity > 0 {
        lines.push(format!("(−{vanity} JC vanity tax.)"));
    }
    chunk_lines(&lines, DISCORD_MESSAGE_LIMIT)
}

fn biggest_prediction_winner(resolution: &Resolution) -> Option<&ResolutionParticipant> {
    let mut participants = resolution.participants.iter().collect::<Vec<_>>();
    // Python sorts profit descending first, then `max(..., key=payout)` keeps
    // the first item on a payout tie. Stable sorting preserves repository order
    // for an exact final tie.
    participants.sort_by_key(|participant| std::cmp::Reverse(participant.profit));
    let mut biggest: Option<&ResolutionParticipant> = None;
    for participant in participants {
        if participant.payout <= 0 {
            continue;
        }
        if biggest.is_none_or(|current| participant.payout > current.payout) {
            biggest = Some(participant);
        }
    }
    biggest
}

fn chunk_lines(lines: &[String], maximum: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in lines {
        if !current.is_empty() && current.len() + 1 + line.len() > maximum {
            chunks.push(std::mem::take(&mut current));
        }
        if line.len() > maximum {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let characters = line.chars().collect::<Vec<_>>();
            chunks.extend(
                characters
                    .chunks(maximum)
                    .map(|chunk| chunk.iter().collect::<String>()),
            );
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

fn resolution_mention_ids(resolution: &Resolution) -> BTreeSet<u64> {
    resolution
        .participants
        .iter()
        .filter_map(|participant| u64::try_from(participant.discord_id).ok())
        .collect()
}

fn message_with_optional_chart(
    content: &str,
    chart: Option<InteractionAttachment>,
    mentions: BTreeSet<u64>,
) -> DiscordMessage {
    let mut response = InteractionResponse::message(content);
    if let Some(chart) = chart {
        response = response.attachment(chart);
    }
    DiscordMessage::mentioning(response, mentions)
}

#[cfg(test)]
#[path = "prediction_provider/tests.rs"]
mod tests;
