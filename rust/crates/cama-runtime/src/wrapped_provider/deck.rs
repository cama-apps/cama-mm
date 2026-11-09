//! The story's shape: at most twenty cards before the recap, Dota first.
//!
//! After the fixed opening (cover, games, play time, top heroes) the
//! sections fill in priority order, each up to its cap: distinct stories,
//! personal records, hero stories, people, side games, and the persona.
//! Slots a section leaves empty roll over to more distinct stories, up to
//! eight, so a Dota-only player still gets a full Dota deck. Everything is
//! deterministic; nothing is left to chance.

use cama_app::wrapped_media::{
    WrappedLayout, WrappedSlideData, WrappedTheme, any_backgrounds_alike, fixed_backgrounds,
    theme_background,
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
pub(super) const MOST_RECORDS: usize = 2;
pub(super) const MOST_HERO_STORIES: usize = 2;
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

/// Fixed cards after a fixed card or a meme punchline of a like color take
/// another palette from their set. Two-beat cards choose their colors
/// afterwards, around these.
pub(super) fn separate_neighbors(slides: &mut [WrappedSlideData]) {
    for index in 1..slides.len() {
        let (before, rest) = slides.split_at_mut(index);
        let (previous, slide) = (&before[index - 1], &mut rest[0]);
        if slide.layout == WrappedLayout::TwoBeat {
            continue;
        }
        let taken = fixed_backgrounds(previous);
        if !any_backgrounds_alike(&taken, &[theme_background(slide.theme)]) {
            continue;
        }
        if let Some(theme) = interchangeable(slide)
            .iter()
            .find(|theme| !any_backgrounds_alike(&taken, &[theme_background(**theme)]))
        {
            slide.theme = *theme;
        }
    }
}

/// Two meme punchlines that read as the same dark color, such as a gold
/// graph's and a cinema still's, cannot trade palettes. When two two-beat
/// cards would sit like that, in order of preference: a later card of the
/// second one's section steps in between; the second card moves back to an
/// earlier gap in its section; or the first card does. A gap is clear of
/// both of its new neighbors. Sections never mix. Fixed cards recolor
/// instead (`separate_neighbors`).
pub(super) fn space_fixed_backgrounds(slides: &mut [WrappedSlideData]) {
    let clash = |slide: &WrappedSlideData, colors: &[[u8; 3]]| {
        any_backgrounds_alike(&fixed_backgrounds(slide), colors)
    };
    // The latest spot before `end` in `kind`'s section where `colors` would
    // clash with neither neighbor.
    let gap = |slides: &[WrappedSlideData], end: usize, kind: &str, colors: &[[u8; 3]]| {
        (0..end).rev().find(|spot| {
            slides[*spot].kind == kind
                && !clash(&slides[*spot], colors)
                && (*spot == 0 || !clash(&slides[spot - 1], colors))
        })
    };
    for index in 1..slides.len() {
        let before = fixed_backgrounds(&slides[index - 1]);
        let current = fixed_backgrounds(&slides[index]);
        if slides[index].layout != WrappedLayout::TwoBeat
            || !any_backgrounds_alike(&before, &current)
        {
            continue;
        }
        let kind = slides[index].kind.clone();
        let later = slides[index + 1..].iter().position(|later| {
            later.kind == kind && !clash(later, &before) && !clash(later, &current)
        });
        if let Some(offset) = later {
            slides[index..=index + 1 + offset].rotate_right(1);
        } else if let Some(spot) = gap(slides, index - 1, &kind, &current) {
            slides[spot..=index].rotate_right(1);
        } else if slides[index - 1].layout == WrappedLayout::TwoBeat
            && index >= 2
            && !clash(&slides[index - 2], &current)
        {
            let previous_kind = slides[index - 1].kind.clone();
            if let Some(spot) = gap(slides, index - 1, &previous_kind, &before) {
                slides[spot..index].rotate_right(1);
            }
        }
    }
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests {
    use cama_app::wrapped_media::{WrappedBeat, WrappedPunch};
    use cama_domain::wrapped_activity::ServerRank;

    use super::*;

    #[test]
    fn distinct_stories_take_what_the_other_sections_leave() {
        let personal = MOST_RECORDS + MOST_HERO_STORIES;
        let fixed = FIXED_KINDS.len();
        let full = fixed + personal + MOST_PEOPLE + MOST_SIDE_GAMES + PERSONA_SLOTS;
        assert_eq!(full + DISTINCT_STORIES, STORY_BUDGET);
        // Every section full: five distinct stories.
        assert_eq!(distinct_slots(full), DISTINCT_STORIES);
        // Three side games instead of four: one slot rolls over.
        assert_eq!(distinct_slots(full - 1), DISTINCT_STORIES + 1);
        // No records or hero stories: eight, never more.
        assert_eq!(distinct_slots(full - personal), MOST_DISTINCT_STORIES);
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

    fn beat(kind: &str, treatment: WrappedPunch) -> WrappedSlideData {
        let mut slide = card(kind);
        slide.layout = WrappedLayout::TwoBeat;
        slide.theme = WrappedTheme::BeatRoast;
        slide.beat = Some(WrappedBeat {
            treatment,
            ..Default::default()
        });
        slide
    }

    #[test]
    fn dark_meme_punchlines_never_sit_together() {
        use WrappedPunch::{Calculated, Cinema, Clock, GoldGraph, Plain, Stonks};
        let treatments = |slides: &[WrappedSlideData]| {
            slides
                .iter()
                .map(|slide| (slide.kind.clone(), slide.beat.as_ref().unwrap().treatment))
                .collect::<Vec<_>>()
        };
        // A black cinema still, a navy gold graph and a near-black clock all
        // read as black; a plain card between them can take any palette.
        let mut deck = vec![
            beat("story_distinct", Cinema),
            beat("story_distinct", GoldGraph),
            beat("story_distinct", Clock),
            beat("story_distinct", Plain),
            beat("story_distinct", Plain),
            beat("story_record", Plain),
        ];
        space_fixed_backgrounds(&mut deck);
        assert_eq!(
            treatments(&deck),
            [
                ("story_distinct".to_owned(), Cinema),
                ("story_distinct".to_owned(), Plain),
                ("story_distinct".to_owned(), GoldGraph),
                ("story_distinct".to_owned(), Plain),
                ("story_distinct".to_owned(), Clock),
                ("story_record".to_owned(), Plain),
            ]
        );
        // The last card of a section that clashes moves back to an earlier
        // gap: a calculated bar's indigo and a gold graph's navy look alike.
        let mut tail = vec![
            beat("story_distinct", Plain),
            beat("story_distinct", Plain),
            beat("story_distinct", Calculated),
            beat("story_distinct", GoldGraph),
            beat("story_record", Plain),
        ];
        space_fixed_backgrounds(&mut tail);
        assert_eq!(
            treatments(&tail)
                .into_iter()
                .map(|(_, treatment)| treatment)
                .collect::<Vec<_>>(),
            [Plain, GoldGraph, Plain, Calculated, Plain]
        );
        // A card alone in its section stays; the card before it moves back
        // instead: a cinema still meeting the Stonks chart.
        let mut across = vec![
            beat("story_distinct", Plain),
            beat("story_distinct", Cinema),
            beat("chart_gamba", Stonks),
        ];
        space_fixed_backgrounds(&mut across);
        assert_eq!(
            treatments(&across)
                .into_iter()
                .map(|(_, treatment)| treatment)
                .collect::<Vec<_>>(),
            [Cinema, Plain, Stonks]
        );
        // Cards never leave their section to make room.
        let mut stuck = vec![
            beat("story_distinct", Cinema),
            beat("story_distinct", GoldGraph),
            beat("story_record", Plain),
        ];
        let before = treatments(&stuck);
        space_fixed_backgrounds(&mut stuck);
        assert_eq!(treatments(&stuck), before);
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
