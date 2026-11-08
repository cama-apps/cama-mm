//! The story's shape: at most twenty cards before the recap, Dota first.
//!
//! After the fixed opening (cover, games, play time, top heroes) the
//! sections fill in priority order, each up to its cap: distinct stories,
//! the slots reserved for personal records and hero stories, people, side
//! games, and the persona. Slots a section leaves empty roll over to more
//! distinct stories, up to eight, so a Dota-only player still gets a full
//! Dota deck. Everything is deterministic; nothing is left to chance.

use cama_app::wrapped_media::{
    WrappedLayout, WrappedSlideData, WrappedTheme, slide_backgrounds, theme_background,
};
use cama_domain::wrapped_activity::ServerRank;

use super::activity::ActivityData;

/// Story cards before the recap.
const STORY_BUDGET: usize = 20;
/// Distinct stories when every other section is full.
#[cfg(all(test, feature = "runtime-test-core"))]
const DISTINCT_STORIES: usize = 5;
/// Distinct stories when other sections leave room.
const MOST_DISTINCT_STORIES: usize = 8;
/// Slots for personal records (two) and hero stories (two). Until those
/// sections exist they hold nothing, and their slots roll over.
pub(super) const RESERVED_SLOTS_USED: usize = 0;
pub(super) const MOST_PEOPLE: usize = 2;
const MOST_SIDE_GAMES: usize = 4;
/// The persona card's slot, held whether or not it is shown.
pub(super) const PERSONA_SLOTS: usize = 1;
/// The opening cards every deck may show.
pub(super) const FIXED_KINDS: [&str; 4] =
    ["story_intro", "story_games", "story_playtime", "story_hero"];
/// Side-game cards a deck shows before any of the others.
const MAIN_SIDE_CARDS: [&str; 4] = ["chart_gamba", "story_dig", "story_pet", "story_mafia"];
/// The side games' meme cards, kept ahead of the remaining extras.
const SIDE_MEMES: [&str; 2] = ["story_dig_cave_in", "story_pet_grave"];

/// How many distinct stories fit beside `used` other story cards.
pub(super) fn distinct_slots(used: usize) -> usize {
    STORY_BUDGET.saturating_sub(used).min(MOST_DISTINCT_STORIES)
}

/// A card's kind once it lives under More stats.
pub(super) fn to_more_stats(kind: &str) -> String {
    format!("more_{kind}")
}

fn side_game(kind: &str) -> Option<usize> {
    match kind {
        "chart_gamba" | "story_gamba" => Some(0),
        "story_dig" | "story_dig_boss" | "story_dig_feud" | "story_dig_cave_in" => Some(1),
        "story_pet" | "story_pet_grave" => Some(2),
        "story_mafia" | "story_mafia_victim" | "story_mafia_accuser" => Some(3),
        _ => None,
    }
}

/// Keeps at most four side-game cards in the story: each game's main card,
/// the most active game first, then the meme cards, then the rest. The
/// others move to More stats. Returns how many stay in the story.
pub(super) fn budget_side_cards(cards: &mut [WrappedSlideData], activity: &ActivityData) -> usize {
    let activity_rank = |rank: Option<ServerRank>| {
        // Hundredths of a percent, so the order is exact.
        rank.and_then(ServerRank::ahead_of_percent)
            .map_or(5_000, |percent| (percent * 100.0).round() as i64)
    };
    let busiest = [
        activity_rank(activity.gamba.rank),
        activity_rank(activity.dig.rank),
        activity_rank(activity.pets.rank),
        activity_rank(activity.mafia.rank),
    ];
    let mut order = cards
        .iter()
        .enumerate()
        .filter(|(_, card)| side_game(&card.kind).is_some())
        .map(|(index, card)| {
            let tier = if MAIN_SIDE_CARDS.contains(&card.kind.as_str()) {
                0
            } else if SIDE_MEMES.contains(&card.kind.as_str()) {
                1
            } else {
                2
            };
            let game = side_game(&card.kind).unwrap_or_default();
            ((tier, -busiest[game], game, index), index)
        })
        .collect::<Vec<_>>();
    order.sort_unstable();
    for (_, index) in order.iter().skip(MOST_SIDE_GAMES) {
        cards[*index].kind = to_more_stats(&cards[*index].kind);
    }
    order.len().min(MOST_SIDE_GAMES)
}

/// Palettes a fixed card can trade without changing its art or meaning.
fn interchangeable(slide: &WrappedSlideData) -> &'static [WrappedTheme] {
    match (slide.kind.as_str(), slide.theme) {
        (_, WrappedTheme::Dig | WrappedTheme::DigDeep) => {
            &[WrappedTheme::Dig, WrappedTheme::DigDeep]
        }
        (_, WrappedTheme::Queue | WrappedTheme::QueueAlt) => {
            &[WrappedTheme::Queue, WrappedTheme::QueueAlt]
        }
        ("story_moment", _) => &super::MOMENT_THEMES,
        ("story_superlative", _) => &[WrappedTheme::Awards, WrappedTheme::Rating],
        _ => &[],
    }
}

/// Fixed cards next to a fixed card of the same color take another palette
/// from their set. Two-beat cards choose their colors afterwards, around
/// these.
pub(super) fn separate_neighbors(slides: &mut [WrappedSlideData]) {
    for index in 1..slides.len() {
        let (before, rest) = slides.split_at_mut(index);
        let (previous, slide) = (&before[index - 1], &mut rest[0]);
        if slide.layout == WrappedLayout::TwoBeat || previous.layout == WrappedLayout::TwoBeat {
            continue;
        }
        let taken = slide_backgrounds(previous);
        if !taken.contains(&theme_background(slide.theme)) {
            continue;
        }
        if let Some(theme) = interchangeable(slide)
            .iter()
            .find(|theme| !taken.contains(&theme_background(**theme)))
        {
            slide.theme = *theme;
        }
    }
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests {
    use cama_domain::wrapped_activity::ServerRank;

    use super::*;

    #[test]
    fn distinct_stories_take_what_the_other_sections_leave() {
        // Records and hero stories, once they exist, take four slots.
        let reserved = 4;
        let fixed = FIXED_KINDS.len();
        let full = fixed + reserved + MOST_PEOPLE + MOST_SIDE_GAMES + PERSONA_SLOTS;
        assert_eq!(full + DISTINCT_STORIES, STORY_BUDGET);
        // Every section full: five distinct stories.
        assert_eq!(distinct_slots(full), DISTINCT_STORIES);
        // Three side games instead of four: one slot rolls over.
        assert_eq!(distinct_slots(full - 1), DISTINCT_STORIES + 1);
        // Records and hero stories not built yet: eight, never more.
        assert_eq!(distinct_slots(full - reserved), MOST_DISTINCT_STORIES);
        // Dota only, no side games: eight.
        let dota_only = fixed + MOST_PEOPLE + PERSONA_SLOTS;
        assert_eq!(distinct_slots(dota_only), MOST_DISTINCT_STORIES);
    }

    fn card(kind: &str) -> WrappedSlideData {
        WrappedSlideData::new(kind, kind)
    }

    #[test]
    fn side_games_keep_main_cards_and_drop_extras_first() {
        let mut cards = [
            "chart_gamba",
            "story_gamba",
            "story_dig",
            "story_dig_boss",
            "story_dig_feud",
            "story_dig_cave_in",
            "story_pet",
            "story_pet_grave",
            "story_mafia",
            "story_mafia_victim",
        ]
        .map(card);
        let activity = ActivityData::default();
        assert_eq!(budget_side_cards(&mut cards, &activity), 4);
        let kept = cards
            .iter()
            .map(|card| card.kind.as_str())
            .filter(|kind| !kind.starts_with("more_"))
            .collect::<Vec<_>>();
        assert_eq!(
            kept,
            ["chart_gamba", "story_dig", "story_pet", "story_mafia"]
        );
        assert!(cards.iter().any(|card| card.kind == "more_story_dig_boss"));

        // A Dig-only year keeps the main card, then Press F, then the most
        // active extras.
        let mut dig = [
            "story_dig",
            "story_dig_boss",
            "story_dig_feud",
            "story_dig_cave_in",
            "story_pet",
            "story_pet_grave",
        ]
        .map(card);
        let mut activity = ActivityData::default();
        activity.dig.rank = Some(ServerRank { rank: 1, of: 20 });
        activity.pets.rank = Some(ServerRank { rank: 9, of: 20 });
        budget_side_cards(&mut dig, &activity);
        let kept = dig
            .iter()
            .map(|card| card.kind.as_str())
            .filter(|kind| !kind.starts_with("more_"))
            .collect::<Vec<_>>();
        assert_eq!(
            kept,
            [
                "story_dig",
                "story_dig_cave_in",
                "story_pet",
                "story_pet_grave"
            ]
        );
    }

    #[test]
    fn fixed_neighbors_trade_palettes_within_their_set() {
        let mut slides = ["story_dig", "story_dig_boss", "story_dig_feud"].map(card);
        for slide in &mut slides {
            slide.layout = WrappedLayout::BigNumber;
            slide.theme = WrappedTheme::DigDeep;
        }
        let mut games = card("story_games");
        games.theme = WrappedTheme::Queue;
        let mut playtime = card("story_playtime");
        playtime.theme = WrappedTheme::Queue;
        let mut deck = vec![games, playtime];
        deck.extend(slides);
        separate_neighbors(&mut deck);
        assert_eq!(deck[1].theme, WrappedTheme::QueueAlt);
        assert_eq!(
            deck[2..]
                .iter()
                .map(|slide| slide.theme)
                .collect::<Vec<_>>(),
            [
                WrappedTheme::DigDeep,
                WrappedTheme::Dig,
                WrappedTheme::DigDeep
            ]
        );
    }
}
