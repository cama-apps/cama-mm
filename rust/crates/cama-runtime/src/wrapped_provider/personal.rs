//! The player against their own year: personal records from single games,
//! their best hero, the hero that hates them, and a Radiant/Dire split.
//! Every card is a two-beat roast or popoff that wears the hero's render.

use cama_app::wrapped_media::WrappedSlideData;
use cama_app::wrapped_story::roast_pools::{hero_copy, record_copy, side_split_copy};
use cama_domain::hero_catalog::bundled_hero_name;
use cama_domain::wrapped_distinct::{
    COUNT_RECORDS, Direction, DistinctMatch, DistinctPlayer, PersonalRecord, hero_year, month_day,
    select_records,
};

use super::activity::ArtSources;
use super::base_slide;
use super::deck::{MOST_HERO_STORIES, MOST_RECORDS};
use super::distinct::{hero_art, set_beat};

/// How many record cards `candidates` can fill.
pub(super) fn record_count(candidates: &[PersonalRecord]) -> usize {
    select_records(candidates, MOST_RECORDS).len()
}

/// The strongest of `target`'s personal records, a roast and a popoff when
/// both exist, dated in the player's time zone. A game another story already
/// shows (`shown`) is not repeated; the next record takes its place.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_story_slides(
    candidates: &[PersonalRecord],
    shown: &[i64],
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
    let fresh = candidates
        .iter()
        .filter(|record| COUNT_RECORDS.contains(&record.key) || !shown.contains(&record.match_id))
        .cloned()
        .collect::<Vec<_>>();
    select_records(&fresh, MOST_RECORDS)
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

/// The best hero, the hero that hates the player, and a Radiant/Dire split,
/// in that order, at most two of them.
pub(super) fn hero_story_slides(
    rows: &[DistinctMatch],
    target: i64,
    art: &ArtSources,
    seed: u64,
    username: &str,
    year_label: &str,
) -> Vec<WrappedSlideData> {
    let year = hero_year(rows, target);
    let heroes = [
        ("hero_best", "Your best hero", Direction::Popoff, &year.best),
        (
            "hero_nemesis",
            "The hero that hates you",
            Direction::Roast,
            &year.nemesis_hero,
        ),
    ]
    .into_iter()
    .filter_map(|(kind, title, direction, line)| {
        let line = line.as_ref()?;
        let copy = hero_copy(kind, line, bundled_hero_name(line.hero_id), seed);
        let mut slide = base_slide("story_hero_year", title, username, year_label);
        slide.art_png = hero_art(art, line.hero_id);
        set_beat(&mut slide, direction, copy);
        Some(slide)
    });
    // A split is about the map, not one hero, so it carries no hero art.
    let split = side_split_copy(&year, seed).map(|copy| {
        let mut slide = base_slide("story_hero_year", "Radiant vs Dire", username, year_label);
        set_beat(&mut slide, Direction::Roast, copy);
        slide
    });
    heroes.chain(split).take(MOST_HERO_STORIES).collect()
}
