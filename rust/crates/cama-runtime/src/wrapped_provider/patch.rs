//! A player's balance changes and earned superlatives, inside the story budget.

use cama_app::wrapped_media::{WrappedLayout, WrappedRow, WrappedSlideData, WrappedTheme};
use cama_app::wrapped_story::personal_patch::{BalanceDirection, PersonalPatch};

use super::base_slide;

const FEATURED_AWARDS: usize = 2;

pub(super) fn patch_slides(
    patch: &PersonalPatch,
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
        slides.push(slide);
    }
    slides
}
