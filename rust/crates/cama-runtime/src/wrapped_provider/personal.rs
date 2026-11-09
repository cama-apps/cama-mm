//! The player against their own year: personal records from single games,
//! their best hero, the hero that hates them, and a Radiant/Dire split.
//! Every card is a two-beat roast or popoff that wears the hero's render.

use cama_app::wrapped_media::WrappedSlideData;
use cama_app::wrapped_story::roast_pools::{
    HERO_STORY_KINDS, StoryCopy, hero_copy, record_copy, side_split_copy,
};
use cama_domain::hero_catalog::bundled_hero_name;
use cama_domain::wrapped_distinct::{
    COUNT_RECORDS, Direction, DistinctMatch, DistinctPlayer, DistinctStory, PersonalRecord,
    hero_year, month_day, record_twin, select_records,
};

use super::activity::ArtSources;
use super::base_slide;
use super::deck::{MOST_HERO_STORIES, MOST_RECORDS};
use super::distinct::{hero_art, set_beat};

/// How many record cards `candidates` can fill.
pub(super) fn record_count(candidates: &[PersonalRecord]) -> usize {
    select_records(candidates, MOST_RECORDS).len()
}

/// The records that tell the deck something new: none about a game a
/// `shown` distinct story is about, or about a measure one already covers,
/// such as a peak-GPM game beside the GPM story. A count spans many games,
/// so only its measure matters.
pub(super) fn fresh_records(
    candidates: &[PersonalRecord],
    shown: &[DistinctStory],
) -> Vec<PersonalRecord> {
    let games: Vec<i64> = shown.iter().filter_map(|story| story.match_id).collect();
    let metrics: Vec<&str> = shown.iter().map(|story| story.metric).collect();
    candidates
        .iter()
        .filter(|record| COUNT_RECORDS.contains(&record.key) || !games.contains(&record.match_id))
        .filter(|record| record_twin(record.key).is_none_or(|twin| !metrics.contains(&twin)))
        .cloned()
        .collect()
}

/// Up to `limit` of the strongest `records`, a roast and a popoff when both
/// exist, dated in the player's time zone.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_story_slides(
    records: &[PersonalRecord],
    limit: usize,
    players: &[DistinctPlayer],
    target: i64,
    art: &ArtSources,
    seed: u64,
    username: &str,
    year_label: &str,
) -> Vec<WrappedSlideData> {
    let timezone = players
        .iter()
        .find(|player| player.discord_id == target)
        .and_then(|player| player.timezone.as_deref());
    select_records(records, limit.min(MOST_RECORDS))
        .iter()
        .map(|record| {
            let hero = record.hero_id.and_then(bundled_hero_name);
            let date = record
                .finished_at
                .and_then(|finished| month_day(finished, timezone));
            let copy = record_copy(record, hero, date.as_deref(), seed);
            let mut slide = base_slide("story_record", "Personal records", username, year_label);
            slide.art_png = record.hero_id.and_then(|hero| hero_art(art, hero));
            set_beat(&mut slide, record.direction, copy);
            slide
        })
        .collect()
}

/// A hero story before its card is drawn.
pub(super) struct HeroStory {
    title: &'static str,
    direction: Direction,
    /// `None` for the Radiant/Dire split, which is about the map.
    hero_id: Option<i64>,
    copy: StoryCopy,
}

/// The best hero, the hero that hates the player, and a Radiant/Dire split,
/// in that order, as far as `target`'s year tells them.
pub(super) fn hero_stories(rows: &[DistinctMatch], target: i64, seed: u64) -> Vec<HeroStory> {
    let year = hero_year(rows, target);
    let [best, nemesis, _] = HERO_STORY_KINDS;
    let heroes = [
        (best, "Your best hero", Direction::Popoff, &year.best),
        (
            nemesis,
            "The hero that hates you",
            Direction::Roast,
            &year.nemesis_hero,
        ),
    ]
    .into_iter()
    .filter_map(|(kind, title, direction, line)| {
        let line = line.as_ref()?;
        Some(HeroStory {
            title,
            direction,
            hero_id: Some(line.hero_id),
            copy: hero_copy(kind, line, bundled_hero_name(line.hero_id), seed),
        })
    });
    let split = side_split_copy(&year, seed).map(|copy| HeroStory {
        title: "Radiant vs Dire",
        direction: Direction::Roast,
        hero_id: None,
        copy,
    });
    heroes.chain(split).collect()
}

/// Up to two of `stories` that tell the deck something new: none about the
/// hero a `shown` hero_loyal story already wears.
pub(super) fn fresh_hero_stories<'a>(
    stories: &'a [HeroStory],
    shown: &[DistinctStory],
) -> Vec<&'a HeroStory> {
    let loyal: Vec<i64> = shown
        .iter()
        .filter(|story| story.metric == "hero_loyal")
        .filter_map(|story| story.hero_id)
        .collect();
    stories
        .iter()
        .filter(|story| story.hero_id.is_none_or(|hero| !loyal.contains(&hero)))
        .take(MOST_HERO_STORIES)
        .collect()
}

/// Cards for the first `limit` of `stories`; each hero's card wears its
/// render.
pub(super) fn hero_story_slides(
    stories: &[&HeroStory],
    limit: usize,
    art: &ArtSources,
    username: &str,
    year_label: &str,
) -> Vec<WrappedSlideData> {
    stories
        .iter()
        .take(limit)
        .map(|story| {
            let mut slide = base_slide("story_hero_year", story.title, username, year_label);
            // A split is about the map, not one hero, so it carries no art.
            slide.art_png = story.hero_id.and_then(|hero| hero_art(art, hero));
            set_beat(&mut slide, story.direction, story.copy.clone());
            slide
        })
        .collect()
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests {
    use cama_domain::wrapped_distinct::{TableEnd, Theme, Tier};

    use super::*;

    /// Player 7's year: five wins in six games on hero 1, all Radiant, and
    /// one win in five on hero 2, all Dire.
    fn year() -> Vec<DistinctMatch> {
        (0..11)
            .map(|index| {
                let on_one = index < 6;
                DistinctMatch {
                    match_id: index,
                    discord_id: 7,
                    side: Some(if on_one { "radiant" } else { "dire" }.to_owned()),
                    won: Some(if on_one { index != 0 } else { index == 6 }),
                    hero_id: Some(if on_one { 1 } else { 2 }),
                    ..DistinctMatch::default()
                }
            })
            .collect()
    }

    fn shown(metric: &'static str, hero: i64) -> DistinctStory {
        DistinctStory {
            metric,
            theme: Theme::Heroes,
            direction: Direction::Popoff,
            tier: Tier::Strong,
            value: 0.5,
            display: "50%".to_owned(),
            rank: None,
            end: TableEnd::Top,
            hero_id: Some(hero),
            match_id: None,
            item: None,
            score: 1.0,
        }
    }

    fn titles(stories: &[&HeroStory]) -> Vec<&'static str> {
        stories.iter().map(|story| story.title).collect()
    }

    #[test]
    fn a_hero_loyal_story_takes_its_heroes_card() {
        let stories = hero_stories(&year(), 7, 3);
        assert_eq!(
            titles(&fresh_hero_stories(&stories, &[])),
            ["Your best hero", "The hero that hates you"]
        );
        // The loyal story already wears hero 1, so the split steps in.
        assert_eq!(
            titles(&fresh_hero_stories(&stories, &[shown("hero_loyal", 1)])),
            ["The hero that hates you", "Radiant vs Dire"]
        );
        assert_eq!(
            titles(&fresh_hero_stories(&stories, &[shown("hero_loyal", 2)])),
            ["Your best hero", "Radiant vs Dire"]
        );
        // Other stories about a game on the hero are no twin.
        assert_eq!(
            titles(&fresh_hero_stories(&stories, &[shown("best_game", 1)])),
            ["Your best hero", "The hero that hates you"]
        );
    }
}
