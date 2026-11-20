//! The patch chapter: the version on the cover, balance changes, hero and
//! item updates, talents, bug fixes and earned superlatives.

use cama_app::wrapped_media::{WrappedLayout, WrappedRow, WrappedSlideData, WrappedTheme};
use cama_app::wrapped_story::bug_fixes::BugFix;
use cama_app::wrapped_story::hero_patch::{HeroChangeTag, HeroUpdate};
use cama_app::wrapped_story::item_patch::{ItemInfo, ItemNote, ItemUpdate};
use cama_app::wrapped_story::personal_patch::{BalanceDirection, PersonalPatch};
use cama_app::wrapped_story::talents::{TalentSide, TalentTier};
use cama_domain::hero_catalog::bundled_hero_name;

use super::activity::ArtSources;
use super::base_slide;

const FEATURED_AWARDS: usize = 2;
/// A player name longer than this does not go in the update's name.
const LONGEST_NAME_IN_TITLE: usize = 14;

/// The year as a game version: 2026 reads `20.26`.
pub(super) fn patch_version(year: i32) -> String {
    format!("{}.{:02}", year.div_euclid(100), year.rem_euclid(100))
}

/// The update's name when no story names it.
pub(super) fn default_patch_name(username: &str) -> String {
    if username.chars().count() <= LONGEST_NAME_IN_TITLE {
        format!("The {username} Update")
    } else {
        "The Annual Update".to_owned()
    }
}

/// The patch-notes section a story card files under.
pub(super) fn section(kind: &str) -> &'static str {
    match kind {
        "story_games" | "story_playtime" | "story_patch" => "GENERAL",
        "story_hero" | "story_hero_patch" | "story_hero_year" => "HERO UPDATES",
        "story_items" => "ITEM UPDATES",
        "story_talents" => "TALENTS",
        "story_distinct" | "story_record" | "story_moment" => "GAMEPLAY",
        "story_bugfixes" => "BUG FIXES",
        "story_patch_award" | "story_superlative" => "SUPERLATIVES",
        "story_people" => "MATCHMAKING",
        "chart_gamba"
        | "story_gamba"
        | "story_dig"
        | "story_dig_boss"
        | "story_dig_feud"
        | "story_dig_cave_in"
        | "story_pet"
        | "story_pet_grave"
        | "story_mafia"
        | "story_mafia_victim"
        | "story_mafia_accuser" => "ARCADE",
        "story_persona" => "PROFILE",
        "story_finale" => "SUMMARY",
        _ => "",
    }
}

/// Every item the game database names, as the item stories read them.
pub(super) fn item_catalog(art: &ArtSources) -> Vec<ItemInfo> {
    art.items()
        .iter()
        .map(|item| ItemInfo {
            key: item.key.clone(),
            name: item.name.clone(),
            cost: item.cost,
        })
        .collect()
}

pub(super) fn patch_slides(
    patch: &PersonalPatch,
    art: &ArtSources,
    username: &str,
    year_label: &str,
) -> Vec<WrappedSlideData> {
    let mut slides = Vec::new();
    if !patch.changes.is_empty() {
        let mut slide = base_slide("story_patch", "Balance changes", username, year_label);
        slide.layout = WrappedLayout::PatchNotes;
        slide.theme = WrappedTheme::Patch;
        slide.headline = "You got\npatched.".to_owned();
        slide.rows = patch
            .changes
            .iter()
            .take(3)
            .map(|change| {
                let (label, sign) = match change.direction {
                    BalanceDirection::Buff => ("Buff", 1.0),
                    BalanceDirection::Nerf => ("Nerf", -1.0),
                };
                let mut row = WrappedRow::new(
                    format!("{label}: {}", change.title),
                    format!("{}\n{}", change.value, change.evidence),
                );
                row.value = sign;
                row
            })
            .collect();
        let buffs = patch
            .changes
            .iter()
            .filter(|change| change.direction == BalanceDirection::Buff)
            .count();
        slide.lines.push(
            if buffs == patch.changes.len() {
                "Nerfs postponed. Enjoy responsibly."
            } else if buffs == 0 {
                "Hotfix requested."
            } else {
                "Perfectly balanced. Allegedly."
            }
            .to_owned(),
        );
        slides.push(slide);
    }
    for (index, award) in patch.awards.iter().enumerate() {
        let kind = if index < FEATURED_AWARDS {
            "story_patch_award"
        } else {
            "more_story_patch_award"
        };
        let mut slide = base_slide(kind, &award.title, username, year_label);
        slide.layout = WrappedLayout::Award;
        slide.theme = if index == 1 {
            WrappedTheme::Awards
        } else {
            WrappedTheme::Patch
        };
        slide.stat_value = award.value.clone();
        slide.stat_label = award.evidence.clone();
        slide.art_png = art.item_icon(award.icon);
        slides.push(slide);
    }
    slides
}

/// The player's heroes as a patch would list them: a verdict, the change
/// lines, and the hero's own ability icons.
pub(super) fn hero_patch_slide(
    updates: &[HeroUpdate],
    art: &ArtSources,
    username: &str,
    year_label: &str,
) -> Option<WrappedSlideData> {
    let rows = updates
        .iter()
        .filter_map(|update| {
            let mut row =
                WrappedRow::new(bundled_hero_name(update.hero_id)?, update.lines.join("\n"));
            let (tag, value) = match update.tag {
                HeroChangeTag::Buffed => ("BUFFED", 1.0),
                HeroChangeTag::Nerfed => ("NERFED", -1.0),
                HeroChangeTag::New => ("NEW", 0.0),
                HeroChangeTag::Removed => ("REMOVED", -1.0),
                HeroChangeTag::Banned => ("BANNED", 0.0),
            };
            row.tag = tag.to_owned();
            row.value = value;
            row.image_png = art.hero_portrait(update.hero_id);
            row.icons = art.ability_icons(update.hero_id);
            Some(row)
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    let mut slide = base_slide("story_hero_patch", "Hero updates", username, year_label);
    slide.headline = if rows.len() == 1 {
        "1 hero rebalanced this year.".to_owned()
    } else {
        format!("{} heroes rebalanced this year.", rows.len())
    };
    slide.rows = rows;
    Some(slide)
}

/// What the player bought, with the shop's own icons.
pub(super) fn item_slide(
    updates: &[ItemUpdate],
    art: &ArtSources,
    username: &str,
    year_label: &str,
) -> Option<WrappedSlideData> {
    if updates.is_empty() {
        return None;
    }
    let mut slide = base_slide("story_items", "Item updates", username, year_label);
    slide.rows = updates
        .iter()
        .map(|update| {
            let mut row = WrappedRow::new(&update.name, &update.line);
            let (tag, value) = match (update.note, update.direction) {
                (ItemNote::Signature, _) => ("SIGNATURE", 0.0),
                (ItemNote::WinRate, Some(BalanceDirection::Nerf)) => ("NERF", -1.0),
                (ItemNote::WinRate, _) => ("BUFF", 1.0),
                (ItemNote::Consumable, _) => ("STOCKED", 0.0),
                (ItemNote::Oddity, _) => ("", 0.0),
            };
            row.tag = tag.to_owned();
            row.value = value;
            row.image_png = art.item_icon(&update.key);
            row
        })
        .collect();
    Some(slide)
}

/// Four either-or talents, each lit on the side the year's results chose.
pub(super) fn talent_slide(
    tiers: &[TalentTier],
    username: &str,
    year_label: &str,
) -> Option<WrappedSlideData> {
    if tiers.is_empty() {
        return None;
    }
    let mut slide = base_slide("story_talents", "Talents", username, year_label);
    slide.rows = tiers
        .iter()
        .map(|tier| {
            let mut row = WrappedRow::new(&tier.left, &tier.right);
            row.tag = tier.level.to_string();
            row.value = match tier.picked {
                TalentSide::Left => -1.0,
                TalentSide::Right => 1.0,
            };
            row
        })
        .collect();
    slide
        .lines
        .push("Picked by your results this year.".to_owned());
    Some(slide)
}

/// The year's roasts as a changelog. A line about a hero the catalog cannot
/// name is left out rather than shown with a blank.
pub(super) fn bug_fix_slide(
    fixes: &[BugFix],
    art: &ArtSources,
    year: i32,
    username: &str,
    year_label: &str,
) -> Option<WrappedSlideData> {
    let rows = fixes
        .iter()
        .filter_map(|fix| {
            let line = match fix.hero_id {
                Some(hero_id) => fix.line.replace("{hero}", bundled_hero_name(hero_id)?),
                None => fix.line.clone(),
            };
            let mut row = WrappedRow::new(line, "");
            if fix.known_issue {
                row.tag = "KNOWN ISSUE".to_owned();
            }
            row.image_png = fix.hero_id.and_then(|hero_id| art.hero_portrait(hero_id));
            Some(row)
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    let mut slide = base_slide("story_bugfixes", "Bug fixes", username, year_label);
    slide.headline = format!("Resolved during the {year} season.");
    slide.rows = rows;
    slide
        .lines
        .push("Thanks to the teammates who reported these.".to_owned());
    Some(slide)
}
