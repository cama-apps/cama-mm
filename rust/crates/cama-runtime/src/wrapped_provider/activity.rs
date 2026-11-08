//! The year outside the match queue: Dig, Camagotchi, gamba, and Mafia
//! chapters, the Cama type, and the shareable recap.

use std::path::PathBuf;

use cama_app::hero_lookup::{HeroImageSize, HeroLookup};
use cama_app::pet_assets::{
    FilesystemPetAssets, HybridPetRenderer, PetAssetLoader, PetRenderRequest,
};
use cama_app::trivia_image_cache::{TriviaImageCache, production_trivia_cache_path};
use cama_app::wrapped_media::{WrappedLayout, WrappedRow, WrappedTheme};
use cama_app::wrapped_story::chapters::{
    CardArt, CardMood, CardStyle, ChapterCard, DigContext, PersonaActivity, PersonaSignal,
    QueueStyle, dig_cards, dig_headline_number, gamba_cards, mafia_cards, persona_card, pet_cards,
    story_seed,
};
use cama_db::gambling_stats_repository::GamblingStatsPort;
use cama_db::wrapped_activity::YearWindow;
use cama_domain::formatting::{format_grouped, format_grouped_signed};
use cama_domain::wrapped_activity::{
    DigYear, GambaYear, GambleEvent, GambleKind, MafiaYear, PetYear,
};

use super::*;

/// The deepest layers of the mine get the darker palette.
const DEEP_LAYER_DEPTH: i64 = 76;
#[cfg(all(test, feature = "runtime-test-core"))]
const DEFAULT_PET_DECAY_PER_DAY: i64 = 20;

type PetArtLoader = PetAssetLoader<FilesystemPetAssets, HybridPetRenderer>;

/// Read-only game art for the chapters. Missing files leave a card without art.
#[derive(Clone)]
pub(super) struct ArtSources {
    dig_root: PathBuf,
    pets: Arc<Mutex<PetArtLoader>>,
    pet_decay_per_day: i64,
    heroes: Arc<HeroLookup>,
    /// The Steam image cache the trivia warm fills at startup.
    hero_cache: PathBuf,
}

impl ArtSources {
    pub(super) fn production(pet_decay_per_day: i64) -> Self {
        Self {
            dig_root: PathBuf::from(cama_app::dig_runtime::DEFAULT_DIG_ASSET_ROOT),
            pets: Arc::new(Mutex::new(PetAssetLoader::new(
                FilesystemPetAssets::production(),
                HybridPetRenderer::production(),
            ))),
            pet_decay_per_day,
            heroes: Arc::new(HeroLookup::production()),
            hero_cache: production_trivia_cache_path(),
        }
    }

    #[cfg(all(test, feature = "runtime-test-core"))]
    pub(super) fn default_production() -> Self {
        Self::production(DEFAULT_PET_DECAY_PER_DAY)
    }

    /// Art from a checked-out asset tree instead of the runtime image.
    #[cfg(all(test, feature = "runtime-test-core"))]
    pub(super) fn for_assets(root: &std::path::Path) -> Self {
        Self {
            dig_root: root.join("dig"),
            pets: Arc::new(Mutex::new(PetAssetLoader::new(
                FilesystemPetAssets::new(root.join("pets")),
                HybridPetRenderer::new(root.join("pets").join("components")),
            ))),
            pet_decay_per_day: DEFAULT_PET_DECAY_PER_DAY,
            heroes: Arc::new(HeroLookup::production()),
            hero_cache: root.join("no-hero-cache"),
        }
    }

    /// Hero metadata and a portrait cache from a test fixture.
    #[cfg(all(test, feature = "runtime-test-core"))]
    pub(super) fn with_hero_portraits(mut self, heroes: HeroLookup, cache: PathBuf) -> Self {
        self.heroes = Arc::new(heroes);
        self.hero_cache = cache;
        self
    }

    /// The hero's Steam portrait when startup has already cached it. Wrapped
    /// never downloads one; a missing file just leaves the row without art.
    pub(super) fn hero_portrait(&self, hero_id: i64) -> Option<Vec<u8>> {
        let url = self.heroes.image_url(hero_id, HeroImageSize::Full)?;
        let path = TriviaImageCache::new(&self.hero_cache)
            .ok()?
            .path_for_url(&url);
        let bytes = std::fs::read(path).ok()?;
        // Steam portraits are well under this; anything larger is not one.
        (bytes.len() <= 1024 * 1024).then_some(bytes)
    }

    /// The hero's full render when startup has already cached it. Never
    /// downloads; callers fall back to the portrait.
    pub(super) fn hero_render(&self, hero_id: i64) -> Option<Vec<u8>> {
        let url = self.heroes.image_url(hero_id, HeroImageSize::Render)?;
        let path = TriviaImageCache::new(&self.hero_cache)
            .ok()?
            .path_for_url(&url);
        let bytes = std::fs::read(path).ok()?;
        // Renders are about 1 MB; anything over 4 MiB is not one.
        (bytes.len() <= 4 * 1024 * 1024).then_some(bytes)
    }

    fn boss_scene(&self, boss_id: &str, scene: &str) -> Option<Vec<u8>> {
        let slug = cama_app::dig_assets::boss_slug(boss_id)?;
        let bytes = std::fs::read(
            self.dig_root
                .join("bosses")
                .join(format!("{slug}_{scene}.png")),
        )
        .ok()?;
        // Authored scenes are small; anything else is not what we expect here.
        (bytes.len() <= 2 * 1024 * 1024).then_some(bytes)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ActivityData {
    pub(super) dig: DigYear,
    pub(super) pets: PetYear,
    pub(super) mafia: MafiaYear,
    pub(super) gamba: GambaYear,
    /// Scenes for the player's most-fought boss, keyed by scene name.
    pub(super) boss_art: BTreeMap<&'static str, Vec<u8>>,
    pub(super) pet_art: Option<Vec<u8>>,
}

impl WrappedSources {
    pub(super) fn load_activity(
        &self,
        guild_id: i64,
        target_id: i64,
        window: YearWindow,
    ) -> Result<ActivityData, String> {
        let dig = self
            .activity
            .dig_year(guild_id, target_id, window)
            .map_err(|error| error.to_string())?;
        let pets = self
            .activity
            .pet_year(guild_id, target_id, window)
            .map_err(|error| error.to_string())?;
        let mafia = self
            .activity
            .mafia_year(guild_id, target_id, window)
            .map_err(|error| error.to_string())?;
        let (wheel, tips, rank) = self
            .activity
            .gamba_side_year(guild_id, target_id, window)
            .map_err(|error| error.to_string())?;
        let gamba = GambaYear {
            events: self.gamble_events(guild_id, target_id, window)?,
            wheel,
            tips,
            rank,
        };
        let mut boss_art = BTreeMap::new();
        if let Some(boss) = dig.bosses.first() {
            for scene in ["victory", "defeat", "encounter"] {
                if let Some(bytes) = self.art.boss_scene(&boss.boss_id, scene) {
                    boss_art.insert(scene, bytes);
                }
            }
        }
        let pet_art = if pets.pets.iter().any(|pet| pet.is_active) {
            self.active_pet_art(guild_id, target_id)
        } else {
            None
        };
        Ok(ActivityData {
            dig,
            pets,
            mafia,
            gamba,
            boss_art,
            pet_art,
        })
    }

    /// Settled bets, wheel spins, and double-or-nothings inside the year, with
    /// the same profit definitions as the lifetime gamba chart.
    fn gamble_events(
        &self,
        guild_id: i64,
        target_id: i64,
        window: YearWindow,
    ) -> Result<Vec<GambleEvent>, String> {
        let port = self.gambling.port();
        let inside = |at: i64| at >= window.start && at < window.end;
        let mut events = port
            .player_bet_history(target_id, Some(guild_id))
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|bet| inside(bet.bet_time) && bet.outcome != GamblingOutcome::Neutral)
            .map(|bet| GambleEvent {
                at: bet.bet_time,
                profit: bet.profit,
                kind: GambleKind::Bet,
                exposure: bet.effective_bet,
                leverage: bet.leverage,
            })
            .collect::<Vec<_>>();
        events.extend(
            port.wheel_history(target_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|spin| inside(spin.spin_time))
                .map(|spin| GambleEvent {
                    at: spin.spin_time,
                    profit: spin.result,
                    kind: GambleKind::Wheel,
                    exposure: spin.result.saturating_abs(),
                    leverage: 1,
                }),
        );
        events.extend(
            port.double_or_nothing_history(target_id, Some(guild_id))
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|spin| inside(spin.spin_time))
                .map(|spin| {
                    let original = spin.balance_before.saturating_add(spin.cost);
                    GambleEvent {
                        at: spin.spin_time,
                        profit: spin.balance_after.saturating_sub(original),
                        kind: GambleKind::DoubleOrNothing,
                        exposure: original,
                        leverage: 1,
                    }
                }),
        );
        events.sort_by_key(|event| event.at);
        Ok(events)
    }

    fn active_pet_art(&self, guild_id: i64, target_id: i64) -> Option<Vec<u8>> {
        let pet = match self.pets.get_active_pet(target_id, Some(guild_id)) {
            Ok(Some(pet)) => pet,
            Ok(None) => return None,
            Err(error) => {
                warn!(%error, target_id, "wrapped pet lookup failed");
                return None;
            }
        };
        let now = Utc::now().timestamp();
        let mut loader = self.art.pets.lock().ok()?;
        let file = loader.get_pet_card(&PetRenderRequest {
            species_id: &pet.species,
            stage: pet.stage(now),
            mood: pet.mood(now, self.art.pet_decay_per_day),
            seed: pet.pet_id,
            accessory: pet.accessory.as_deref(),
            evolution: crate::pet_flavor_runtime::evolution_visual(&pet),
        });
        let bytes = file.bytes();
        // Only PNG art can be composited; animated authored cards are skipped.
        bytes.starts_with(b"\x89PNG").then(|| bytes.to_vec())
    }
}

impl ActivityData {
    /// Everyone a chapter might name, so their display names are prefetched.
    pub(super) fn person_ids(&self) -> impl Iterator<Item = i64> + '_ {
        [
            self.dig.favorite_target,
            self.dig.top_saboteur,
            self.pets.brawl_rival,
            self.mafia.favorite_victim,
            self.mafia.top_accuser,
            self.gamba.tips.top_recipient,
        ]
        .into_iter()
        .flatten()
        .map(|tally| tally.discord_id)
    }
}

fn boss_name(boss_id: &str) -> Option<String> {
    cama_app::dig_bosses::boss_by_id(boss_id).map(|boss| boss.name.to_owned())
}

/// Every chapter card for the year outside the queue, in story order.
pub(super) fn activity_cards(
    activity: &ActivityData,
    degen: Option<(i64, &str)>,
    bankruptcies: i64,
    names: &dyn Fn(i64) -> Option<String>,
    target_id: i64,
    year: i32,
) -> Vec<ChapterCard> {
    let seed = story_seed(target_id, year);
    let mut cards = gamba_cards(&activity.gamba, degen, bankruptcies, names, seed);
    let deepest = (activity.dig.deepest > 0)
        .then(|| cama_app::dig_service::layer_at(activity.dig.deepest).name);
    cards.extend(dig_cards(
        &activity.dig,
        &DigContext {
            boss_name: &boss_name,
            deepest_layer: deepest,
            deep: activity.dig.deepest >= DEEP_LAYER_DEPTH,
        },
        names,
        seed,
    ));
    cards.extend(pet_cards(&activity.pets, names, seed));
    cards.extend(mafia_cards(&activity.mafia, names, seed));
    cards
}

pub(super) fn card_slide(
    card: ChapterCard,
    activity: &ActivityData,
    username: &str,
    year_label: &str,
) -> WrappedSlideData {
    let mut slide = base_slide(card.kind, &card.title, username, year_label);
    slide.layout = match card.style {
        CardStyle::BigNumber => WrappedLayout::BigNumber,
        CardStyle::Tiles => WrappedLayout::Tiles,
        CardStyle::Spotlight => WrappedLayout::Spotlight,
        CardStyle::Bars => WrappedLayout::Bars,
        CardStyle::Curve => WrappedLayout::Curve,
        CardStyle::Persona => WrappedLayout::Persona,
    };
    slide.theme = match card.mood {
        CardMood::Months => WrappedTheme::Months,
        CardMood::GambaUp => WrappedTheme::GambaUp,
        CardMood::GambaDown => WrappedTheme::GambaDown,
        CardMood::Dig => WrappedTheme::Dig,
        CardMood::DigDeep => WrappedTheme::DigDeep,
        CardMood::Pet => WrappedTheme::Pet,
        CardMood::PetGrave => WrappedTheme::PetGrave,
        CardMood::Mafia => WrappedTheme::Mafia,
        CardMood::MafiaRed => WrappedTheme::MafiaRed,
        CardMood::Persona => WrappedTheme::Persona,
    };
    slide.art_png = match &card.art {
        CardArt::None => None,
        CardArt::BossVictory(_) => activity.boss_art.get("victory").cloned(),
        CardArt::BossDefeat(_) => activity.boss_art.get("defeat").cloned(),
        CardArt::BossEncounter(_) => activity.boss_art.get("encounter").cloned(),
        CardArt::ActivePet => activity.pet_art.clone(),
    };
    slide.headline = card.headline;
    slide.stat_value = card.stat_value;
    slide.stat_label = card.stat_label;
    slide.badge = card.badge;
    slide.lines = card.lines;
    slide.rows = card
        .rows
        .into_iter()
        .enumerate()
        .map(|(index, (primary, secondary))| WrappedRow {
            primary,
            secondary,
            value: card.series.get(index).copied().unwrap_or_default(),
            image_png: None,
        })
        .collect();
    if card.style == CardStyle::Curve {
        slide.series = card.series;
    }
    slide
}

/// Each activity's claim on the player's identity, strongest evidence first.
pub(super) fn persona_signals(
    personal: Option<&PersonalSummaryWrapped>,
    has_cohort: bool,
    identity: Option<&str>,
    activity: &ActivityData,
    minimum_games: usize,
) -> Vec<PersonaSignal> {
    let strength = |rank: Option<cama_domain::wrapped_activity::ServerRank>| {
        rank.and_then(|rank| rank.ahead_of_percent())
            .unwrap_or(50.0)
    };
    let mut signals = Vec::new();
    if let Some(personal) =
        personal.filter(|personal| personal.games_played >= minimum_games as i64)
    {
        let style = match identity {
            Some("Team player") => QueueStyle::Enabler,
            Some("High kills") => QueueStyle::Headliner,
            Some("Wards") => QueueStyle::Mapmaker,
            Some("Hero specialist") => QueueStyle::Loyalist,
            Some("Hero variety") => QueueStyle::Sampler,
            _ => QueueStyle::Unknown,
        };
        signals.push(PersonaSignal {
            activity: PersonaActivity::Queue(style),
            strength: if has_cohort {
                personal.games_played_percentile
            } else {
                50.0
            },
            evidence: (
                format_grouped(personal.games_played),
                "games played".to_owned(),
            ),
        });
    }
    let dig = &activity.dig;
    if dig.digs >= 5 {
        signals.push(PersonaSignal {
            activity: PersonaActivity::Dig,
            strength: strength(dig.rank),
            evidence: {
                let (value, label) = dig_headline_number(dig);
                (value, label.to_owned())
            },
        });
    }
    // Pets rank by this year's spending; their feedings are lifetime totals.
    let pets = &activity.pets;
    if pets.jc_spent > 0 {
        signals.push(PersonaSignal {
            activity: PersonaActivity::Pets,
            strength: strength(pets.rank),
            evidence: (
                format!("{} JC", format_grouped(pets.jc_spent)),
                "on pet care".to_owned(),
            ),
        });
    }
    let events = &activity.gamba.events;
    if events.len() >= 3 {
        let net = events.iter().map(|event| event.profit).sum::<i64>();
        signals.push(PersonaSignal {
            activity: if net >= 0 {
                PersonaActivity::GambaUp
            } else {
                PersonaActivity::GambaDown
            },
            strength: strength(activity.gamba.rank),
            evidence: (
                format!("{} JC", format_grouped_signed(net)),
                "from gamba".to_owned(),
            ),
        });
    }
    let mafia = &activity.mafia;
    if mafia.games >= 2 {
        signals.push(PersonaSignal {
            activity: if mafia.mafia_wins > 0 {
                PersonaActivity::MafiaLiar
            } else {
                PersonaActivity::MafiaTown
            },
            strength: strength(mafia.rank),
            evidence: (format_grouped(mafia.games), "Mafia games".to_owned()),
        });
    }
    signals
}

/// The Cama type: a persona title from the distinct stories when there are
/// any, over each activity's evidence.
pub(super) fn persona_slide(
    signals: &[PersonaSignal],
    persona: Option<&(String, String)>,
    activity: &ActivityData,
    username: &str,
    year_label: &str,
) -> Option<WrappedSlideData> {
    let card = persona_card(signals).map(|card| card_slide(card, activity, username, year_label));
    let Some((title, subtitle)) = persona else {
        return card;
    };
    let mut slide = card.unwrap_or_else(|| {
        let mut slide = base_slide("story_persona", "", username, year_label);
        slide.layout = WrappedLayout::Persona;
        slide.theme = WrappedTheme::Persona;
        slide
    });
    slide.title = "Your Cama type".to_owned();
    slide.stat_value.clone_from(title);
    slide.headline.clone_from(subtitle);
    slide.lines = vec![subtitle.clone()];
    Some(slide)
}

/// The share card's "numbers" column: `value|label` pairs, most personal
/// first. `highlights` are the best popoff and roast, already in that form.
pub(super) fn recap_numbers(
    personal: Option<&PersonalSummaryWrapped>,
    playtime: Option<&str>,
    highlights: &[String],
    activity: &ActivityData,
) -> Vec<String> {
    let mut numbers = Vec::new();
    if let Some(personal) = personal {
        numbers.push(format!(
            "{}|games played",
            format_grouped(personal.games_played)
        ));
    }
    if let Some(time) = playtime {
        numbers.push(format!("{time}|in recorded matches"));
    }
    numbers.extend(highlights.iter().cloned());
    if activity.dig.digs >= 5 {
        let (value, label) = dig_headline_number(&activity.dig);
        numbers.push(format!("{value}|{label}"));
    }
    if activity.gamba.events.len() >= 3 {
        let net = activity
            .gamba
            .events
            .iter()
            .map(|event| event.profit)
            .sum::<i64>();
        numbers.push(format!("{} JC|from gamba", format_grouped_signed(net)));
    }
    if activity.mafia.games >= 2 {
        numbers.push(format!(
            "{}|Mafia games",
            format_grouped(activity.mafia.games)
        ));
    }
    if activity.pets.jc_spent > 0 {
        numbers.push(format!(
            "{} JC|on pet care",
            format_grouped(activity.pets.jc_spent)
        ));
    }
    numbers.truncate(5);
    numbers
}
