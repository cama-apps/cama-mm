//! Discord-independent Camagotchi pet presentation: the embeds, the status
//! button view, and the service port the live pet provider and sweep worker
//! render through.

use std::collections::BTreeMap;

use cama_domain::formatting::{JOPACOIN_EMOTE, format_grouped};
use cama_domain::pet::{
    DIG_WORK_CAP_BLOCKS, DIG_WORK_UNITS_PER_BLOCK, FEED_CAP_PER_DAY, FOOD_ITEMS, FeedOutcome,
    GILDED_EGG_PREMIUM, Pet, PetMood, PetStage, PetStatus, RefundNotice, SALT_LICK,
    SOLO_TRAINING_SESSION_CAP, SPECIES, SpeciesTier, TrinketOutcome, UNHATCHED_SPECIES,
    WARNING_HUNGER, food_cost, get_accessory, get_species,
};
use cama_domain::pet_evolution::{
    PetCalling, PetInstinct, calling_profile, hint_text, instinct_label,
};
use cama_domain::service_result::ServiceResult;

use crate::pet::{AdoptOutcome, BuyOutcome, SacrificeOutcome, SacrificePreview, SweepOutcome};

const DAY_SECONDS: i64 = 86_400;
const STATUS_VIEW_TIMEOUT_SECONDS: u64 = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedColor {
    Blue,
    Green,
    Gold,
    Orange,
    Red,
    Slate,
    Custom(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embed {
    pub title: String,
    pub description: String,
    pub color: EmbedColor,
    pub fields: Vec<EmbedField>,
    pub image: Option<String>,
    pub footer: Option<String>,
}

impl Embed {
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        color: EmbedColor,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            color,
            fields: Vec::new(),
            image: None,
            footer: None,
        }
    }

    pub fn field(&mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline,
        });
    }

    #[must_use]
    pub fn field_value(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.value.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusAction {
    Feed(String),
    Buy(String),
    SaltLick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusButton {
    pub label: String,
    pub disabled: bool,
    pub action: StatusAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusView {
    pub owner_id: i64,
    pub guild_id: Option<i64>,
    pub timeout_seconds: u64,
    pub buttons: Vec<StatusButton>,
}

impl StatusView {
    #[must_use]
    pub fn new(
        owner_id: i64,
        guild_id: Option<i64>,
        supplies: Option<&BTreeMap<String, i64>>,
        species_id: &str,
        can_feed: bool,
    ) -> Self {
        let empty = BTreeMap::new();
        let supplies = supplies.unwrap_or(&empty);
        let mut buttons = Vec::with_capacity(FOOD_ITEMS.len() * 2 + 1);
        for food in FOOD_ITEMS {
            let owned = supplies.get(food.item_id).copied().unwrap_or_default();
            buttons.push(StatusButton {
                label: format!("Feed {} ×{owned}", food.display_name),
                disabled: !can_feed || owned <= 0,
                action: StatusAction::Feed(food.item_id.to_owned()),
            });
        }
        for food in FOOD_ITEMS {
            buttons.push(StatusButton {
                label: format!(
                    "Buy {} ({})",
                    food.display_name,
                    food_cost(species_id, food.item_id).unwrap_or(food.cost)
                ),
                disabled: false,
                action: StatusAction::Buy(food.item_id.to_owned()),
            });
        }
        buttons.push(StatusButton {
            label: format!(
                "Salt Lick ({})",
                food_cost(species_id, SALT_LICK.item_id).unwrap_or(SALT_LICK.cost)
            ),
            disabled: false,
            action: StatusAction::SaltLick,
        });
        Self {
            owner_id,
            guild_id,
            timeout_seconds: STATUS_VIEW_TIMEOUT_SECONDS,
            buttons,
        }
    }
}

/// Command-facing contract.  Return values are shared with the already ported
/// pet application service, while command-only reads/writes remain explicit so
/// a live adapter cannot silently invent behavior.
pub trait PetCommandService {
    fn decay_per_day(&self) -> i64;
    fn sweep(&mut self) -> Result<SweepOutcome, String>;
    fn mark_hatch_announced(&mut self, pet: &Pet) -> Result<(), String>;
    fn mark_evolution_announced(&mut self, pet: &Pet) -> Result<(), String>;
    fn mark_death_announced(&mut self, pet: &Pet) -> Result<(), String>;
    fn mark_refund_announced(&mut self, notice: &RefundNotice) -> Result<(), String>;
    fn adopt(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
        egg_tier: &str,
    ) -> ServiceResult<AdoptOutcome>;
    fn stable(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<Vec<Pet>>;
    fn activate(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        pet_id: i64,
    ) -> ServiceResult<cama_db::pet_repository::ActivatePetOutcome>;
    fn upgrade(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        pet_id: i64,
    ) -> ServiceResult<Pet>;
    fn sacrifice_preview(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<SacrificePreview>;
    fn sacrifice(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        name: &str,
    ) -> ServiceResult<SacrificeOutcome>;
    fn status(&mut self, discord_id: i64, guild_id: Option<i64>) -> ServiceResult<PetStatus>;
    fn next_adoption_fee(&mut self, discord_id: i64, guild_id: Option<i64>) -> i64;
    fn balance(&mut self, discord_id: i64, guild_id: Option<i64>) -> Option<i64>;
    fn buy(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
        quantity: i64,
    ) -> ServiceResult<BuyOutcome>;
    fn feed(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        item_id: &str,
    ) -> ServiceResult<FeedOutcome>;
    fn roll_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
    ) -> ServiceResult<TrinketOutcome>;
    fn wear_trinket(
        &mut self,
        discord_id: i64,
        guild_id: Option<i64>,
        accessory_id: &str,
    ) -> ServiceResult<String>;
    fn owned_trinkets(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<String>;
    fn rename(&mut self, discord_id: i64, guild_id: Option<i64>, name: &str) -> ServiceResult<Pet>;
    fn graveyard(&mut self, discord_id: i64, guild_id: Option<i64>) -> Vec<Pet>;
    fn leaderboard(&mut self, guild_id: Option<i64>) -> Vec<Pet>;
}

#[must_use]
pub fn build_adoption_embed(outcome: &AdoptOutcome, owner_name: &str) -> Embed {
    let gilded = outcome.egg_tier == "gilded";
    let title = if gilded {
        "🥚 A gilded egg!"
    } else {
        "🥚 A mysterious egg!"
    };
    let description = if outcome.upgraded {
        format!(
            "**{owner_name}** upgraded their egg to a **gilded egg** and named it **{}** for {GILDED_EGG_PREMIUM} {JOPACOIN_EMOTE}.\nIt still hatches <t:{}:R>. What's inside? Nobody knows. Not even the egg.",
            outcome.pet.name, outcome.pet.hatched_at
        )
    } else {
        let flair = if gilded { "a **gilded egg**" } else { "an egg" };
        let pity = if outcome.pity_active {
            "\n✨ The nonprofit took pity: this egg can't be Common."
        } else {
            ""
        };
        let hatch = if outcome.pet.is_active {
            format!("It hatches <t:{}:R>.", outcome.pet.hatched_at)
        } else {
            "It is waiting safely in the stable; its hatch clock resumes when activated.".to_owned()
        };
        format!(
            "**{owner_name}** adopted {flair} and named it **{}** for {} {JOPACOIN_EMOTE}.\n{hatch} What's inside? Nobody knows. Not even the egg.{pity}",
            outcome.pet.name, outcome.pet.adopt_fee
        )
    };
    let mut embed = Embed::new(title, description, EmbedColor::Gold);
    embed.image = Some(format!("attachment://pet-egg-{}.png", outcome.pet.pet_id));
    embed
}

#[must_use]
pub fn build_altar_preview_embed(preview: &SacrificePreview) -> Embed {
    let weights = preview.weights;
    let odds = [
        ("Common", weights.common),
        ("Uncommon", weights.uncommon),
        ("Rare", weights.rare),
        ("Legendary", weights.legendary),
    ]
    .into_iter()
    .map(|(tier, weight)| format!("{tier} {weight}%"))
    .collect::<Vec<_>>()
    .join(" · ");
    Embed::new(
        format!("🩸 The altar hungers for {}", preview.pet.name),
        format!(
            "Sacrifice your {} for an immediate reroll with upgraded odds:\n**{odds}**\n\nFee: **{}** {JOPACOIN_EMOTE} — and this death still escalates your future adoption fees. The altar gives, but it remembers.",
            pet_species_label(&preview.pet),
            preview.fee
        ),
        EmbedColor::Slate,
    )
}

#[must_use]
pub fn build_altar_cancel_embed(preview: &SacrificePreview) -> Embed {
    Embed::new(
        "🕯️ The altar goes cold",
        format!("**{}** lives to graze another day.", preview.pet.name),
        EmbedColor::Slate,
    )
}

#[must_use]
pub fn build_altar_success_embed(outcome: &SacrificeOutcome) -> Embed {
    let mut embed = Embed::new(
        format!("🩸 {} was given to the altar", outcome.dead_pet.name),
        format!(
            "The altar accepts **{}** ({}).\nIn the ashes, a new egg: **{}**, hatching <t:{}:R> — its species already chosen, already hidden.\nFee: {} {JOPACOIN_EMOTE}.",
            outcome.dead_pet.name,
            pet_species_label(&outcome.dead_pet),
            outcome.new_pet.name,
            outcome.new_pet.hatched_at,
            outcome.fee,
        ),
        EmbedColor::Gold,
    );
    embed.image = Some(format!(
        "attachment://pet-altar-{}.png",
        outcome.dead_pet.pet_id
    ));
    embed
}

#[must_use]
pub fn build_status_embed(
    status: &PetStatus,
    decay_per_day: i64,
    now: i64,
    owner_name: &str,
    next_fee: i64,
) -> Embed {
    build_status_embed_with_details(StatusEmbedRequest {
        status,
        decay_per_day,
        now,
        owner_name,
        next_fee,
        brawl_record: None,
        career: None,
        solo_training_sessions: None,
    })
}

/// All facts needed to render the enriched live status panel.  Keeping the
/// request typed makes it difficult for the runtime adapter to mix fields from
/// different snapshots and keeps the projection API below its argument limit.
pub struct StatusEmbedRequest<'a> {
    pub status: &'a PetStatus,
    pub decay_per_day: i64,
    pub now: i64,
    pub owner_name: &'a str,
    pub next_fee: i64,
    pub brawl_record: Option<(i64, i64)>,
    pub career: Option<&'a crate::pet_brawl::CareerSummary>,
    pub solo_training_sessions: Option<i64>,
}

/// Build the status panel with the optional brawl/career projections that the
/// pet command loads alongside the pet status row.  The simple wrapper
/// above remains for callers that do not compose the brawl service.
#[must_use]
pub fn build_status_embed_with_details(request: StatusEmbedRequest<'_>) -> Embed {
    let StatusEmbedRequest {
        status,
        decay_per_day,
        now,
        owner_name,
        next_fee,
        brawl_record,
        career,
        solo_training_sessions,
    } = request;
    let Some(pet) = status.pet.as_ref() else {
        return if let Some(dead) = status.last_dead.as_ref() {
            let species = get_species(&dead.species);
            let calling = pet_calling_label(dead);
            let mut identity = pet_species_label(dead);
            if let Some(calling) = calling {
                identity.push_str(&format!(" · {calling}"));
            }
            let died_at = dead.died_at.unwrap_or(dead.adopted_at);
            let death_phrase = if dead.death_cause.as_deref() == Some("eaten") {
                "was eaten".to_owned()
            } else {
                format!(
                    "died of {}",
                    dead.death_cause.as_deref().unwrap_or("starvation")
                )
            };
            let mut embed = Embed::new(
                format!("In memoriam: {}", dead.name),
                format!(
                    "{identity} · {death_phrase}, {} — <t:{died_at}:R>\n\n_{}_\n\nF.\n\nAdopt again with `/pet adopt` — {next_fee} {JOPACOIN_EMOTE}",
                    format_age(dead.age_seconds(died_at)),
                    species.blurb,
                ),
                EmbedColor::Slate,
            );
            embed.image = Some(format!("attachment://pet-tombstone-{}.png", dead.pet_id));
            embed
        } else {
            Embed::new(
                format!("{owner_name} has no cama"),
                format!(
                    "A hollow feeling. A camaless existence.\n\nAdopt a mysterious egg with `/pet adopt` — {next_fee} {JOPACOIN_EMOTE}"
                ),
                EmbedColor::Blue,
            )
        };
    };
    if status.stage == Some(PetStage::Egg) {
        let mut embed = Embed::new(
            format!("🥚 {}", pet.name),
            format!(
                "A mysterious egg, warm to the touch.\nHatches <t:{}:R>. What's inside? Nobody knows.",
                pet.hatched_at
            ),
            EmbedColor::Gold,
        );
        embed.field("Supplies", supplies_line(status.supplies.as_ref()), false);
        embed.image = Some(format!("attachment://pet-egg-{}.png", pet.pet_id));
        return embed;
    }
    let mood = status.mood.unwrap_or(PetMood::Content);
    let species = get_species(&pet.species);
    let profile = pet
        .evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .map(calling_profile);
    let color = profile.map_or_else(
        || {
            if species.tier == SpeciesTier::Legendary {
                EmbedColor::Custom(0xE9_1E_63)
            } else {
                match mood {
                    PetMood::Happy => EmbedColor::Green,
                    PetMood::Content => EmbedColor::Blue,
                    PetMood::Hungry => EmbedColor::Orange,
                    PetMood::Starving => EmbedColor::Red,
                }
            }
        },
        |profile| EmbedColor::Custom(profile.color),
    );
    let calling = profile.map_or_else(String::new, |profile| {
        format!(" · {} {}", profile.emoji, profile.label)
    });
    let mut embed = Embed::new(
        format!("{} — {}{}", pet.name, pet_species_label(pet), calling),
        format!("_{}_", species.blurb),
        color,
    );
    // Keep the same compact title-case stage text as discord.py.
    let stage = status.stage.unwrap_or(PetStage::Baby);
    embed.field(
        "Stage",
        format!(
            "{} · {}",
            stage_title(stage),
            format_age(status.age_seconds)
        ),
        true,
    );
    let mut mood_text = format!("{} {}", mood_emoji(mood), stage_title_mood(mood));
    if pet.pampered_until.is_some_and(|until| until > now) {
        mood_text.push_str(" (🧂 pampered)");
    }
    embed.field("Mood", mood_text, true);
    embed.field(
        "Fullness",
        format!("`{}` {}/100", hunger_bar(status.hunger), status.hunger),
        false,
    );
    embed.field(
        "Mining",
        format!(
            "{}/{} blocks banked · +{}/day",
            format_work_blocks(status.dig_work_units),
            DIG_WORK_CAP_BLOCKS,
            status.dig_work_rate
        ),
        false,
    );
    if let Some(upbringing) = hint_text(status.evolution_hint.as_deref()) {
        embed.field("Upbringing", upbringing, false);
    }
    if let Some(profile) = profile {
        embed.field(
            "Calling",
            format!("_{}_\nPaths: {}", profile.blurb, pet_calling_paths(pet)),
            false,
        );
    }
    if status.hunger <= WARNING_HUNGER {
        let starvation_at = pet
            .starvation_time(decay_per_day)
            .unwrap_or(now.saturating_add(DAY_SECONDS));
        embed.field(
            "⚠️ Warning",
            format!("Will starve <t:{starvation_at}:R> without food!"),
            false,
        );
    }
    embed.field("Supplies", supplies_line(status.supplies.as_ref()), false);
    if let Some(accessory) = pet.accessory.as_deref() {
        let accessory = get_accessory(accessory);
        embed.field(
            "Trinket",
            format!("{} {}", accessory.emoji, accessory.display_name),
            true,
        );
    }
    if let Some((wins, losses)) = brawl_record
        && (wins != 0 || losses != 0)
    {
        embed.field("⚔️ Brawls", format!("{wins}W · {losses}L"), true);
    }
    if let Some(career) = career {
        let titles = [career.rank.as_deref(), career.build.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let title_line = if titles.is_empty() {
            String::new()
        } else {
            format!("\n{titles}")
        };
        let level = career.strength + career.intelligence + career.dexterity;
        let solo_line = solo_training_sessions.map_or_else(String::new, |sessions| {
            format!("\nSolo: {sessions}/{SOLO_TRAINING_SESSION_CAP} banked · `/pet train`")
        });
        embed.field(
            "🏋️ Training",
            format!(
                "Level {level}/4 · {}/20 XP\nSTR {} · INT {} · DEX {}{solo_line}{title_line}",
                career.xp, career.strength, career.intelligence, career.dexterity,
            ),
            false,
        );
    }
    embed.footer = Some(format!(
        "{}/{} feeds left today",
        (FEED_CAP_PER_DAY
            - pet.feeds_used_on(
                &cama_domain::game_date::game_date_for_timestamp(now as f64).unwrap_or_default(),
            ))
        .max(0),
        FEED_CAP_PER_DAY
    ));
    embed.image = Some(format!("attachment://pet-{}.png", pet.pet_id));
    embed
}

fn mood_emoji(mood: PetMood) -> &'static str {
    match mood {
        PetMood::Happy => "😊",
        PetMood::Content => "🙂",
        PetMood::Hungry => "😟",
        PetMood::Starving => "😫",
    }
}

fn stage_title(stage: PetStage) -> &'static str {
    match stage {
        PetStage::Egg => "Egg",
        PetStage::Baby => "Baby",
        PetStage::Adult => "Adult",
    }
}

fn stage_title_mood(mood: PetMood) -> &'static str {
    match mood {
        PetMood::Happy => "Happy",
        PetMood::Content => "Content",
        PetMood::Hungry => "Hungry",
        PetMood::Starving => "Starving",
    }
}

#[must_use]
pub fn build_shop_embed(
    supplies: Option<&BTreeMap<String, i64>>,
    species_id: &str,
    balance: Option<i64>,
) -> Embed {
    let mut embed = Embed::new(
        "🛒 Cama Pantry",
        "Buy with `/pet buy` or the buttons on `/pet status`.",
        EmbedColor::Blue,
    );
    for food in FOOD_ITEMS {
        let cost = food_cost(species_id, food.item_id).unwrap_or(food.cost);
        let discount = if cost < food.cost {
            " (discounted!)"
        } else {
            ""
        };
        let owned = supplies
            .and_then(|stock| stock.get(food.item_id))
            .copied()
            .unwrap_or_default();
        embed.field(
            format!(
                "{} {} — {cost} {JOPACOIN_EMOTE}{discount}",
                food.emoji, food.display_name
            ),
            format!(
                "+{} fullness · owned ×{owned}\n_{}_",
                food.restore, food.blurb
            ),
            false,
        );
    }
    let lick_cost = food_cost(species_id, SALT_LICK.item_id).unwrap_or(SALT_LICK.cost);
    embed.field(
        format!(
            "{} {} — {lick_cost} {JOPACOIN_EMOTE}",
            SALT_LICK.emoji, SALT_LICK.display_name
        ),
        format!(
            "Applies immediately: pampered mood for 12h.\n_{}_",
            SALT_LICK.blurb
        ),
        false,
    );
    if let Some(balance) = balance {
        embed.footer = Some(format!("Balance: {balance} jopacoin"));
    }
    embed
}

/// `owner_name` is the owner's readable display text, never a mention.
#[must_use]
pub fn build_hatch_embed(pet: &Pet, owner_name: &str) -> Embed {
    let species = get_species(&pet.species);
    let reveal = match species.tier {
        SpeciesTier::Common => "It's a",
        SpeciesTier::Uncommon => "Oh! It's a",
        SpeciesTier::Rare => "✨ Incredible — it's a",
        SpeciesTier::Legendary => "🎆 UNBELIEVABLE — it's the",
    };
    let mut embed = Embed::new(
        format!("🐣 {} hatched!", pet.name),
        format!(
            "{owner_name}'s egg cracked open... **{reveal} {}!**\n\n_{}_",
            species.display_name, species.blurb
        ),
        if species.tier == SpeciesTier::Legendary {
            EmbedColor::Custom(0xE9_1E_63)
        } else {
            EmbedColor::Green
        },
    );
    embed.image = Some(format!("attachment://pet-{}.png", pet.pet_id));
    embed
}

/// `owner_name` is the owner's readable display text, never a mention.
#[must_use]
pub fn build_evolution_embed(pet: &Pet, owner_name: &str) -> Embed {
    let calling = pet
        .evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .unwrap_or(PetCalling::Wayfarer);
    let profile = calling_profile(calling);
    let paths = pet_calling_paths(pet);
    let mut embed = Embed::new(
        format!(
            "{} {} evolved — {}!",
            profile.emoji, pet.name, profile.label
        ),
        format!(
            "{owner_name}'s {} found a calling.\n\n**{paths}**\n_{}_",
            pet_species_label(pet),
            profile.blurb
        ),
        EmbedColor::Custom(profile.color),
    );
    embed.image = Some(format!("attachment://pet-{}.png", pet.pet_id));
    embed
}

/// `owner_name` is the owner's readable display text, never a mention.
#[must_use]
pub fn build_death_embed(pet: &Pet, owner_name: &str) -> Embed {
    let cause = match pet.death_cause.as_deref() {
        Some("sacrifice") => "was given to the altar",
        Some("eaten") => "was eaten",
        _ => "starved",
    };
    let calling = pet_calling_label(pet);
    let calling = calling.map_or_else(String::new, |label| format!(" · {label}"));
    let died_at = pet.died_at.unwrap_or(pet.adopted_at);
    let mut embed = Embed::new(
        format!("🪦 {} has died", pet.name),
        format!(
            "{owner_name}'s {}{calling} {cause}, {}.\nTime of death: <t:{died_at}:f>. F.",
            pet_species_label(pet),
            format_age(pet.age_seconds(died_at)),
        ),
        EmbedColor::Slate,
    );
    embed.image = Some(format!("attachment://pet-tombstone-{}.png", pet.pet_id));
    embed
}

/// Short parenthetical for the non-zero shares withheld from a payout, e.g.
/// ` (−3 JC bankruptcy, −1 JC vanity)`; empty when nothing was withheld.
#[must_use]
pub fn withholding_note(bankruptcy: i64, vanity: i64, low_priority: i64) -> String {
    let parts: Vec<String> = [
        (bankruptcy, "bankruptcy"),
        (vanity, "vanity"),
        (low_priority, "low priority"),
    ]
    .into_iter()
    .filter(|(amount, _)| *amount > 0)
    .map(|(amount, label)| format!("−{amount} JC {label}"))
    .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

#[must_use]
pub fn build_eating_outcome_embed(pet: &Pet, outcome: &BTreeMap<String, i64>) -> Embed {
    let withheld = |key: &str| outcome.get(key).copied().unwrap_or_default();
    let (bankruptcy, vanity, low_priority) = (
        withheld("bankruptcy_penalty"),
        withheld("vanity_tax"),
        withheld("low_priority_tax"),
    );
    let reward =
        outcome.get("reward").copied().unwrap_or_default() - bankruptcy - vanity - low_priority;
    let note = withholding_note(bankruptcy, vanity, low_priority);
    let added = outcome
        .get("penalty_games_added")
        .copied()
        .unwrap_or_default();
    let remaining = outcome
        .get("penalty_games_remaining")
        .copied()
        .unwrap_or_default();
    let balance = outcome.get("new_balance").copied().unwrap_or_default();
    Embed::new(
        format!("🍖 {} was delicious", pet.name),
        format!(
            "You received **{}** {JOPACOIN_EMOTE}{note}.\n\nBad karma adds **{added}** game(s) to your sentence (**{remaining} remaining**). Your future earnings are taxed until you win your way free.\n\nBalance: **{}** {JOPACOIN_EMOTE}.",
            format_grouped(reward),
            format_grouped(balance)
        ),
        EmbedColor::Slate,
    )
}

fn parse_calling(value: &str) -> Option<PetCalling> {
    PetCalling::ALL
        .into_iter()
        .find(|calling| calling.as_str() == value)
}

fn parse_instinct(value: &str) -> Option<PetInstinct> {
    PetInstinct::ALL
        .into_iter()
        .find(|instinct| instinct.as_str() == value)
}

fn pet_species_label(pet: &Pet) -> String {
    let species = get_species(&pet.species);
    let badge = match species.tier {
        SpeciesTier::Common => "",
        SpeciesTier::Uncommon => "🔹 ",
        SpeciesTier::Rare => "🔮 ",
        SpeciesTier::Legendary => "👑 ",
    };
    format!("{badge}{}", species.display_name)
}

fn pet_calling_label(pet: &Pet) -> Option<String> {
    pet.evolution_calling
        .as_deref()
        .and_then(parse_calling)
        .map(|calling| {
            let profile = calling_profile(calling);
            format!("{} {}", profile.emoji, profile.label)
        })
}

fn pet_calling_paths(pet: &Pet) -> String {
    let paths = [
        pet.evolution_primary.as_deref(),
        pet.evolution_secondary.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(parse_instinct)
    .map(instinct_label)
    .collect::<Vec<_>>();
    if paths.is_empty() {
        "A little of every path".to_owned()
    } else {
        paths.join(" + ")
    }
}

#[must_use]
pub fn build_refund_embed(notice: &RefundNotice, owner_name: &dyn Fn(i64) -> String) -> Embed {
    let mut payouts = notice.payouts.clone();
    payouts.sort_by_key(|payout| std::cmp::Reverse(payout.amount));
    let lines = payouts
        .iter()
        .take(15)
        .map(|payout| {
            format!(
                "{} spent {} on care → rolled **{}%** → +{} {JOPACOIN_EMOTE}",
                owner_name(payout.discord_id),
                payout.consumed_jc,
                payout.multiplier_pct,
                payout.amount
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let scaled = if notice.scaled_down {
        "\n\n_The nonprofit couldn't cover the full total — refunds were scaled down pro-rata._"
    } else {
        ""
    };
    Embed::new(
        format!("🏦 Weekly pet care refund — {}", notice.week_key),
        format!("The jopacoin nonprofit rewards responsible cama ownership.\n\n{lines}{scaled}"),
        EmbedColor::Green,
    )
}

/// A read-only welfare audit. Dormant stable pets have frozen care clocks;
/// pending starvation is reported as critical hunger until death is recorded.
#[must_use]
pub fn build_aspca_embed(
    pets: &[Pet],
    decay_per_day: i64,
    now: i64,
    owner_name: &dyn Fn(i64) -> String,
) -> Embed {
    #[derive(Default)]
    struct Charges {
        hungry: usize,
        critical: usize,
        dead: usize,
        eaten: usize,
        sacrificed: usize,
    }
    impl Charges {
        fn score(&self) -> usize {
            self.hungry + self.critical * 3 + self.dead * 5 + self.sacrificed * 8 + self.eaten * 10
        }
    }
    let mut owners: BTreeMap<i64, Charges> = BTreeMap::new();
    let mut incidents: BTreeMap<i64, Vec<(i64, Option<&str>)>> = BTreeMap::new();
    for pet in pets {
        if let Some(died_at) = pet.died_at {
            incidents
                .entry(pet.discord_id)
                .or_default()
                .push((died_at, pet.death_cause.as_deref()));
            let charges = owners.entry(pet.discord_id).or_default();
            match pet.death_cause.as_deref() {
                Some("eaten") => charges.eaten += 1,
                Some("sacrifice") => charges.sacrificed += 1,
                _ => charges.dead += 1,
            }
        } else if pet.is_active
            && pet.species != UNHATCHED_SPECIES
            && pet.stage(now) != PetStage::Egg
        {
            let hunger = pet.current_hunger(now, decay_per_day);
            if hunger <= WARNING_HUNGER {
                let charges = owners.entry(pet.discord_id).or_default();
                if hunger == 0 {
                    charges.critical += 1;
                } else {
                    charges.hungry += 1;
                }
            }
        }
    }
    let mut ranked = owners.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(owner, charges)| (std::cmp::Reverse(charges.score()), *owner));
    let mut embed = Embed::new(
        "🚨 ASPCA · Camagotchi Hall of Shame",
        "**Association for Seriously Peeved Camagotchi Advocates**\n\
         For just one feeding a day, you could stop being the subject of this report.",
        EmbedColor::Red,
    );
    if ranked.is_empty() {
        embed.description.push_str("\n\n🕊️ No charges today. Bowls are topped up, the altar is quiet, and nobody has confused a companion with a kebab. Inspectors remain suspicious.");
        return embed;
    }
    let totals = ranked.iter().fold([0_usize; 5], |mut sums, (_, c)| {
        for (sum, count) in
            sums.iter_mut()
                .zip([c.hungry, c.critical, c.dead, c.eaten, c.sacrificed])
        {
            *sum += count;
        }
        sums
    });
    embed.field("📋 Server case file", format!(
        "{} hungry · {} at zero hunger · {} other deaths · {} eaten · {} sacrificed\n{} owners on file. The sad violin budget has been exceeded.",
        totals[0], totals[1], totals[2], totals[3], totals[4], ranked.len()
    ), false);
    for (index, (owner, c)) in ranked.iter().take(10).enumerate() {
        let mut evidence = Vec::new();
        if c.hungry > 0 {
            evidence.push(format!(
                "🥣 **{} hungry** — the bowl is decorative, apparently.",
                c.hungry
            ));
        }
        if c.critical > 0 {
            evidence.push(format!(
                "🆘 **{} at zero hunger** — emergency inspection. Bring food, not excuses.",
                c.critical
            ));
        }
        if c.dead > 0 {
            evidence.push(format!(
                "🪦 **{} deaths on record** — the adoption desk remembers.",
                c.dead
            ));
        }
        if c.eaten > 0 {
            evidence.push(format!(
                "🍽️ **{} eaten** — ‘a forever home’ was your digestive tract.",
                c.eaten
            ));
        }
        if c.sacrificed > 0 {
            evidence.push(format!(
                "🕯️ **{} sacrificed** — traded unconditional love for better gacha odds.",
                c.sacrificed
            ));
        }
        let verdict = if c.eaten > 0 {
            "Your pet was a friend, not a meal plan."
        } else if c.sacrificed > 0 {
            "The altar has requested a restraining order."
        } else if c.dead > 0 {
            "Your references are all tombstones."
        } else {
            "Sentenced to learning where the feed button is."
        };
        embed.field(
            format!("#{} · {} shame points", index + 1, c.score()),
            format!(
                "{}\n{}\n*{verdict}*{}",
                owner_name(*owner),
                evidence.join("\n"),
                cama_domain::pet::adoption_suspended_until(
                    incidents.get(owner).into_iter().flatten().copied(),
                    now,
                )
                .map_or_else(String::new, |until| format!(
                    "\n🔒 **Adoption suspended** until <t:{until}:f> (<t:{until}:R>)."
                ))
            ),
            false,
        );
    }
    embed.footer = Some(format!(
        "Showing {} of {} owners • Shame: hungry 1 / zero hunger 3 / death 5 / sacrifice 8 / eaten 10 • Suspension: each 10 death points within 7 days = 1 day, max 7 • /pet feed",
        ranked.len().min(10),
        ranked.len()
    ));
    embed
}

#[must_use]
pub fn build_graveyard_embed(pets: &[Pet], owner_name: &str) -> Embed {
    build_graveyard_embed_with_dex(pets, owner_name, None, None)
}

#[must_use]
pub fn build_graveyard_embed_with_dex(
    pets: &[Pet],
    owner_name: &str,
    camadex: Option<(&[String], usize)>,
    callingdex: Option<(&[PetCalling], usize)>,
) -> Embed {
    let mut embed = if pets.is_empty() {
        Embed::new(
            format!("🪦 {owner_name}'s graveyard"),
            "No pets have died. A spotless record. So far.",
            EmbedColor::Blue,
        )
    } else {
        let lines = pets
            .iter()
            .map(|pet| {
                let calling = pet_calling_label(pet)
                    .map_or_else(String::new, |calling| format!(" · {calling}"));
                let died_at = pet.died_at.unwrap_or(pet.adopted_at);
                format!(
                    "**{}** · {}{calling} · {} · <t:{died_at}:R>",
                    pet.name,
                    pet_species_label(pet),
                    format_age(pet.age_seconds(died_at)),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Embed::new(
            format!("🪦 {owner_name}'s graveyard"),
            format!("{lines}\n\nF."),
            EmbedColor::Slate,
        )
    };
    if let Some((raised, total)) = camadex {
        let discovered = SPECIES
            .iter()
            .filter(|species| raised.iter().any(|raised| raised == species.species_id))
            .count();
        let entries = SPECIES
            .iter()
            .map(|species| {
                if raised.iter().any(|raised| raised == species.species_id) {
                    species.display_name.to_owned()
                } else {
                    "???".to_owned()
                }
            })
            .collect::<Vec<_>>();
        embed.field(
            format!("📖 Camadex {discovered}/{total}"),
            entries.join(" · "),
            false,
        );
    }
    if let Some((raised, total)) = callingdex {
        let discovered = PetCalling::ALL
            .into_iter()
            .filter(|calling| raised.contains(calling))
            .count();
        let entries = PetCalling::ALL
            .into_iter()
            .map(|calling| {
                if raised.contains(&calling) {
                    calling_profile(calling).label.to_owned()
                } else {
                    "???".to_owned()
                }
            })
            .collect::<Vec<_>>();
        embed.field(
            format!("Callings {discovered}/{total}"),
            entries.join(" · "),
            false,
        );
    }
    embed
}

#[must_use]
pub fn build_leaderboard_embed(
    pets: &[Pet],
    decay_per_day: i64,
    now: i64,
    owner_name: &dyn Fn(i64) -> String,
) -> Embed {
    build_leaderboard_embed_with_records(pets, decay_per_day, now, None, owner_name)
}

#[must_use]
pub fn build_leaderboard_embed_with_records(
    pets: &[Pet],
    decay_per_day: i64,
    now: i64,
    records: Option<&BTreeMap<i64, (i64, i64)>>,
    owner_name: &dyn Fn(i64) -> String,
) -> Embed {
    if pets.is_empty() {
        return Embed::new(
            "🏆 Oldest living camas",
            "No pets have hatched yet. `/pet adopt` to be first.",
            EmbedColor::Blue,
        );
    }
    let lines = pets
        .iter()
        .enumerate()
        .map(|(index, pet)| {
            let mood = pet.mood(now, decay_per_day);
            let rank = match index {
                0 => "🥇".to_owned(),
                1 => "🥈".to_owned(),
                2 => "🥉".to_owned(),
                _ => format!("{}.", index + 1),
            };
            let record = records
                .and_then(|records| records.get(&pet.pet_id))
                .copied()
                .unwrap_or_default();
            let record_text = if record.0 != 0 || record.1 != 0 {
                format!(" · ⚔️ {}W-{}L", record.0, record.1)
            } else {
                String::new()
            };
            format!(
                "{rank} **{}** ({}) — {} · {} {}{record_text}",
                pet.name,
                pet_species_label(pet),
                owner_name(pet.discord_id),
                format_age(pet.age_seconds(now)),
                mood_emoji(mood),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Embed::new("🏆 Oldest living camas", lines, EmbedColor::Green)
}

#[must_use]
pub fn hunger_bar(hunger: i64) -> String {
    let clamped = hunger.clamp(0, 100);
    let quotient = clamped / 10;
    let remainder = clamped % 10;
    let rounded = quotient + i64::from(remainder > 5 || (remainder == 5 && quotient % 2 != 0));
    let filled = usize::try_from(rounded.clamp(0, 10)).unwrap_or_default();
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

fn supplies_line(supplies: Option<&BTreeMap<String, i64>>) -> String {
    let Some(supplies) = supplies else {
        return "None — visit `/pet shop`".to_owned();
    };
    let parts = FOOD_ITEMS
        .iter()
        .filter_map(|food| {
            let quantity = supplies.get(food.item_id).copied().unwrap_or_default();
            (quantity > 0).then(|| format!("{} {} ×{quantity}", food.emoji, food.display_name))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "None — visit `/pet shop`".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn format_age(age_seconds: i64) -> String {
    let days = age_seconds / DAY_SECONDS;
    if days >= 1 {
        return format!("{days} day{} old", if days == 1 { "" } else { "s" });
    }
    let hours = age_seconds / 3_600;
    format!("{hours} hour{} old", if hours == 1 { "" } else { "s" })
}

fn format_work_blocks(units: i64) -> String {
    let tenths = units.saturating_mul(10) / DIG_WORK_UNITS_PER_BLOCK;
    if tenths % 10 == 0 {
        (tenths / 10).to_string()
    } else {
        format!("{}.{}", tenths / 10, tenths.abs() % 10)
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use cama_domain::pet::{EGG_HATCH_SECONDS, PetBase};

    use super::*;

    const T0: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;
    const GUILD: i64 = 999;
    const OWNER: i64 = 100;

    fn make_pet() -> Pet {
        Pet::new(PetBase {
            pet_id: 1,
            discord_id: 111,
            guild_id: GUILD,
            name: "Blep".to_owned(),
            species: "common_cama".to_owned(),
            adopted_at: T0,
            hatched_at: T0 + EGG_HATCH_SECONDS,
            adopt_fee: 20,
            last_fed_at: T0 + EGG_HATCH_SECONDS,
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
            dig_work_at: T0 + EGG_HATCH_SECONDS,
            aegis_used: 0,
            hatch_announced_at: None,
            died_at: None,
            death_cause: None,
            death_announced_at: None,
        })
    }

    fn owner_pet() -> Pet {
        let mut pet = make_pet();
        pet.discord_id = OWNER;
        pet
    }

    #[test]
    fn aspca_distinguishes_causes_and_pending_starvation() {
        let now = T0 + 10 * DAY;
        let mut pets = Vec::new();
        for cause in [Some("eaten"), Some("sacrifice"), Some("starvation"), None] {
            let mut pet = owner_pet();
            pet.died_at = Some(now - DAY);
            pet.death_cause = cause.map(str::to_owned);
            pets.push(pet);
        }
        let mut hungry = owner_pet();
        hungry.last_fed_at = now;
        hungry.hunger_at_last_fed = WARNING_HUNGER;
        pets.push(hungry);
        pets.push(owner_pet()); // Zero hunger, but no persisted death yet.
        let embed = build_aspca_embed(&pets, 20, now, &|id| format!("Owner {id}"));
        let charges = &embed.fields[1];
        assert_eq!(charges.name, "#1 · 32 shame points");
        for text in [
            "1 hungry",
            "1 at zero hunger",
            "2 deaths on record",
            "1 eaten",
            "1 sacrificed",
            "Owner 100",
        ] {
            assert!(charges.value.contains(text), "missing {text}");
        }
        assert!(charges.value.contains("Adoption suspended"));
    }

    #[test]
    fn aspca_exempts_eggs_stabled_and_well_fed_pets() {
        let now = T0 + 10 * DAY;
        let mut stabled = owner_pet();
        stabled.is_active = false;
        let mut egg = owner_pet();
        egg.species = UNHATCHED_SPECIES.to_owned();
        let mut future_hatch = owner_pet();
        future_hatch.hatched_at = now + DAY;
        let mut healthy = owner_pet();
        healthy.last_fed_at = now;
        let embed = build_aspca_embed(&[stabled, egg, future_hatch, healthy], 20, now, &|id| {
            format!("Owner {id}")
        });
        assert!(embed.description.contains("No charges today"));
        assert!(embed.fields.is_empty());
    }

    #[test]
    fn aspca_ranks_deterministically_and_fits_discord_embed_limits() {
        let now = T0 + 10 * DAY;
        let mut pets = Vec::new();
        for owner in (1..=20).rev() {
            for cause in ["eaten", "sacrifice", "starvation"] {
                let mut pet = owner_pet();
                pet.discord_id = owner;
                pet.died_at = Some(now);
                pet.death_cause = Some(cause.to_owned());
                pets.push(pet);
            }
            let mut pet = owner_pet();
            pet.discord_id = owner;
            pets.push(pet.clone());
            pet.last_fed_at = now;
            pet.hunger_at_last_fed = WARNING_HUNGER;
            pets.push(pet);
        }
        let embed = build_aspca_embed(&pets, 20, now, &|id| format!("Owner {id}"));
        assert_eq!(embed.fields.len(), 11);
        assert!(embed.fields[1].value.starts_with("Owner 1\n"));
        assert!(embed.footer.as_ref().unwrap().contains("10 of 20"));
        let mut length = embed.title.chars().count()
            + embed.description.chars().count()
            + embed.footer.as_ref().unwrap().chars().count();
        for field in &embed.fields {
            assert!(field.value.chars().count() <= 1024);
            length += field.name.chars().count() + field.value.chars().count();
        }
        assert!(length <= 6000, "embed has {length} characters");
    }

    #[test]
    fn test_eating_outcome_embed_shows_the_net_reward_and_withholdings() {
        let pet = make_pet();
        let outcome = BTreeMap::from([
            ("reward".to_owned(), 888),
            ("penalty_games_added".to_owned(), 4),
            ("penalty_games_remaining".to_owned(), 7),
            ("new_balance".to_owned(), 1_884),
            ("bankruptcy_penalty".to_owned(), 3),
            ("vanity_tax".to_owned(), 1),
            ("low_priority_tax".to_owned(), 0),
        ]);

        let embed = build_eating_outcome_embed(&pet, &outcome);

        assert!(
            embed.description.starts_with(&format!(
                "You received **884** {JOPACOIN_EMOTE} (−3 JC bankruptcy, −1 JC vanity)."
            )),
            "{}",
            embed.description
        );
        assert!(embed.description.contains("Balance: **1,884**"));
        assert_eq!(withholding_note(0, 0, 0), "");
        assert_eq!(withholding_note(0, 0, 2), " (−2 JC low priority)");
    }

    #[test]
    fn production_hatch_evolution_and_death_embeds_match_the_python_presentation() {
        let mut legendary = make_pet();
        legendary.species = "sunspun_cama".to_owned();
        let hatch = build_hatch_embed(&legendary, "Owner Name");
        assert!(
            hatch
                .description
                .starts_with("Owner Name's egg cracked open")
        );
        assert!(!hatch.description.contains("<@"));
        assert_eq!(hatch.color, EmbedColor::Custom(0xE9_1E_63));
        assert!(hatch.description.contains("🎆 UNBELIEVABLE — it's the"));
        assert!(hatch.description.contains("Sunspun Cama"));
        assert!(hatch.description.contains("Sun-warm threads"));
        assert_eq!(hatch.image.as_deref(), Some("attachment://pet-1.png"));

        legendary.evolution_calling = Some("oracle".to_owned());
        legendary.evolution_primary = Some("fortune".to_owned());
        legendary.evolution_secondary = Some("wisdom".to_owned());
        let evolution = build_evolution_embed(&legendary, "Owner Name");
        assert!(evolution.description.starts_with("Owner Name's "));
        assert_eq!(evolution.title, "🔮 Blep evolved — Oracle!");
        assert_eq!(
            evolution.color,
            EmbedColor::Custom(calling_profile(PetCalling::Oracle).color)
        );
        assert!(evolution.description.contains("👑 Sunspun Cama"));
        assert!(evolution.description.contains("**Fortune + Wisdom**"));
        assert!(evolution.description.contains("Reads tomorrow in patterns"));
        assert_eq!(evolution.image.as_deref(), Some("attachment://pet-1.png"));

        legendary.died_at = Some(legendary.hatched_at + 9 * DAY);
        legendary.death_cause = Some("starvation".to_owned());
        let death = build_death_embed(&legendary, "Owner Name");
        assert!(death.description.starts_with("Owner Name's "));
        assert!(death.description.contains("👑 Sunspun Cama · 🔮 Oracle"));
        assert!(death.description.contains("starved, 9 days old"));
        assert!(
            death
                .description
                .contains(&format!("<t:{}:f>", legendary.hatched_at + 9 * DAY))
        );
        assert_eq!(
            death.image.as_deref(),
            Some("attachment://pet-tombstone-1.png")
        );
    }

    #[test]
    fn dead_status_embed_preserves_python_memorial_copy_and_tombstone() {
        let mut dead = make_pet();
        dead.species = "sunspun_cama".to_owned();
        dead.evolution_calling = Some("oracle".to_owned());
        dead.died_at = Some(dead.hatched_at + 9 * DAY);
        dead.death_cause = Some("eaten".to_owned());
        let status = PetStatus {
            pet: None,
            hunger: 0,
            stage: None,
            mood: None,
            age_seconds: 0,
            supplies: None,
            last_dead: Some(dead.clone()),
            dig_work_units: 0,
            dig_work_rate: 0,
            evolution_hint: None,
        };

        let embed = build_status_embed(&status, 20, dead.died_at.unwrap(), "Owner", 40);

        assert_eq!(embed.title, "In memoriam: Blep");
        assert_eq!(
            embed.description,
            format!(
                "👑 Sunspun Cama · 🔮 Oracle · was eaten, 9 days old — <t:{}:R>\n\n_{}_\n\nF.\n\nAdopt again with `/pet adopt` — 40 {JOPACOIN_EMOTE}",
                dead.died_at.unwrap(),
                get_species("sunspun_cama").blurb,
            )
        );
        assert_eq!(
            embed.image.as_deref(),
            Some("attachment://pet-tombstone-1.png")
        );
    }

    #[test]
    fn test_status_embed_shows_training_stats_rank_and_build() {
        let pet = make_pet();
        let status = PetStatus {
            pet: Some(pet.clone()),
            hunger: 55,
            stage: Some(PetStage::Baby),
            mood: Some(PetMood::Content),
            age_seconds: DAY,
            supplies: None,
            last_dead: None,
            dig_work_units: 0,
            dig_work_rate: 0,
            evolution_hint: None,
        };
        let career = crate::pet_brawl::CareerSummary {
            xp: 20,
            strength: 2,
            intelligence: 1,
            dexterity: 1,
            rank: Some("Champion".to_owned()),
            build: Some("Bruiser".to_owned()),
        };

        let embed = build_status_embed_with_details(StatusEmbedRequest {
            status: &status,
            decay_per_day: 20,
            now: pet.hatched_at + DAY,
            owner_name: "Owner",
            next_fee: 20,
            brawl_record: None,
            career: Some(&career),
            solo_training_sessions: Some(3),
        });

        assert_eq!(
            embed.field_value("🏋️ Training"),
            Some(
                "Level 4/4 · 20/20 XP\nSTR 2 · INT 1 · DEX 1\nSolo: 3/3 banked · `/pet train`\nChampion · Bruiser"
            )
        );
    }

    #[test]
    fn test_living_status_embed_shows_the_upbringing_hint() {
        let pet = make_pet();
        let status = PetStatus {
            pet: Some(pet.clone()),
            hunger: 55,
            stage: Some(PetStage::Baby),
            mood: Some(PetMood::Content),
            age_seconds: DAY,
            supplies: None,
            last_dead: None,
            dig_work_units: 0,
            dig_work_rate: 0,
            evolution_hint: Some("delving_strong".to_owned()),
        };

        let embed = build_status_embed(&status, 20, pet.hatched_at + DAY, "Owner", 20);

        assert_eq!(
            embed.field_value("Upbringing"),
            hint_text(Some("delving_strong"))
        );
    }

    #[test]
    fn test_living_status_embed_shows_passive_mining_progress() {
        let pet = make_pet();
        let status = PetStatus {
            pet: Some(pet.clone()),
            hunger: 55,
            stage: Some(PetStage::Baby),
            mood: Some(PetMood::Content),
            age_seconds: DAY,
            supplies: None,
            last_dead: None,
            dig_work_units: 12 * DAY + DAY / 2,
            dig_work_rate: 8,
            evolution_hint: None,
        };

        let embed = build_status_embed(&status, 20, pet.hatched_at + DAY, "Owner", 20);

        assert_eq!(embed.field_value("Fullness"), Some("`██████░░░░` 55/100"));
        assert_eq!(
            embed.field_value("Mining"),
            Some("12.5/36 blocks banked · +8/day")
        );
    }

    #[test]
    fn test_food_ui_describes_positive_fullness() {
        let choices = FOOD_ITEMS
            .iter()
            .map(|food| {
                format!(
                    "{} ({} JC, +{} fullness)",
                    food.display_name, food.cost, food.restore
                )
            })
            .collect::<Vec<_>>();
        assert!(
            choices
                .iter()
                .all(|choice| choice.contains("fullness") && !choice.contains("hunger"))
        );
        let shop = build_shop_embed(None, "common_cama", Some(100));
        assert!(
            shop.fields[..3].iter().all(|field| {
                field.value.contains("fullness") && !field.value.contains("hunger")
            })
        );
    }

    #[test]
    fn test_feed_button_disabled_without_stock() {
        let supplies = BTreeMap::from([("tango".to_owned(), 0)]);
        let view = StatusView::new(OWNER, Some(GUILD), Some(&supplies), "common_cama", true);
        let feed_buttons = view
            .buttons
            .iter()
            .filter(|button| matches!(button.action, StatusAction::Feed(_)))
            .collect::<Vec<_>>();
        assert!(!feed_buttons.is_empty());
        assert!(feed_buttons.iter().all(|button| button.disabled));
    }

    #[test]
    fn altar_embeds_preserve_exact_preview_cancel_and_success_copy() {
        let mut offering = owner_pet();
        offering.name = "Offering".to_owned();
        let tier = get_species(&offering.species).tier;
        let preview = SacrificePreview {
            pet: offering.clone(),
            tier,
            was_adult: false,
            fee: 50,
            weights: cama_domain::pet::sacrifice_tier_weights(tier, false),
        };

        let offer = build_altar_preview_embed(&preview);
        assert_eq!(offer.title, "🩸 The altar hungers for Offering");
        assert!(
            offer
                .description
                .contains("**Common 55% · Uncommon 33% · Rare 11% · Legendary 1%**")
        );
        assert!(
            offer
                .description
                .contains(&format!("Fee: **50** {JOPACOIN_EMOTE}"))
        );
        assert_eq!(offer.color, EmbedColor::Slate);

        let cancelled = build_altar_cancel_embed(&preview);
        assert_eq!(cancelled.title, "🕯️ The altar goes cold");
        assert_eq!(
            cancelled.description,
            "**Offering** lives to graze another day."
        );

        let mut dead_pet = offering;
        dead_pet.died_at = Some(dead_pet.hatched_at);
        dead_pet.death_cause = Some("sacrifice".to_owned());
        let mut new_pet = owner_pet();
        new_pet.pet_id = 2;
        new_pet.name = "Rebirth".to_owned();
        let outcome = SacrificeOutcome {
            dead_pet,
            new_pet,
            fee: preview.fee,
            tier,
            was_adult: false,
        };
        let success = build_altar_success_embed(&outcome);
        assert_eq!(success.title, "🩸 Offering was given to the altar");
        assert!(
            success
                .description
                .contains("The altar accepts **Offering**")
        );
        assert!(success.description.contains("a new egg: **Rebirth**"));
        assert!(
            success
                .description
                .contains("its species already chosen, already hidden")
        );
        assert_eq!(success.color, EmbedColor::Gold);
        assert_eq!(
            success.image.as_deref(),
            Some("attachment://pet-altar-1.png")
        );
    }
}
