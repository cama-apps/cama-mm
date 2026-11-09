//! What makes the player different: their most distinctive numbers this year
//! as two-beat roast and popoff cards, the people they shared the year with,
//! and the persona those numbers add up to. Also the two side-game cards that
//! land a meme: the gamba Stonks chart and the cave-in Press F.

use std::collections::VecDeque;

use cama_app::wrapped_media::{
    WrappedBeat, WrappedLayout, WrappedPunch, WrappedSlideData, WrappedTheme,
};
use cama_app::wrapped_story::chapters::{PRESS_F_CAVE_INS, side_tier};
use cama_app::wrapped_story::roast_pools::{
    CopyVars, Punch, StoryCopy, people_copy, persona_title, side_game_copy, story_copy,
};
use cama_domain::formatting::format_grouped;
use cama_domain::hero_catalog::bundled_hero_name;
use cama_domain::wrapped_activity::{DigYear, GambaYear};
use cama_domain::wrapped_distinct::{
    Direction, DistinctMatch, DistinctPlayer, DistinctStory, PeopleYear, Theme, distinct_stories,
    select_stories,
};

use super::activity::ArtSources;
use super::base_slide;

/// A deck with fewer distinct stories also shows the personal moment cards.
pub(super) const MIN_STORIES: usize = 2;
/// Persona pills: the strongest stories behind the title.
const PERSONA_EVIDENCE: usize = 3;

pub(super) struct DistinctDeck {
    /// `story_distinct` two-beat cards, alternating roasts and popoffs.
    pub stories: Vec<WrappedSlideData>,
    /// Built from the stories, when there are any.
    pub persona: Option<Persona>,
    /// `value|label` recap lines for the best popoff and the best roast.
    pub best_popoff: Option<String>,
    pub best_roast: Option<String>,
    /// The best popoff's fact line, which the recap wears as its badge.
    pub badge: Option<String>,
    /// The games and metric keys the stories are about, so other cards can
    /// avoid repeating one.
    pub games: Vec<i64>,
    pub metrics: Vec<&'static str>,
}

pub(super) struct Persona {
    pub title: String,
    pub subtitle: String,
    /// `(value, label)` of the strongest stories, best first.
    pub evidence: Vec<(String, String)>,
}

/// Up to `max_stories` distinct cards for `target`, ranked against everyone
/// in `rows` with at least `minimum_games` games. `gold` returns a match's
/// radiant gold advantage per minute when it was parsed.
#[allow(clippy::too_many_arguments)]
pub(super) fn distinct_slides(
    rows: &[DistinctMatch],
    players: &[DistinctPlayer],
    target: i64,
    minimum_games: usize,
    max_stories: usize,
    art: &ArtSources,
    gold: &dyn Fn(i64) -> Option<Vec<f64>>,
    seed: u64,
    username: &str,
    year_label: &str,
) -> DistinctDeck {
    let candidates = distinct_stories(rows, players, target, minimum_games);
    let selected = select_stories(&candidates, max_stories);
    let (mut best_popoff, mut best_roast, mut badge) = (None, None, None);
    let mut evidence = Vec::new();
    let mut cards = Vec::with_capacity(selected.len());
    for story in &selected {
        let vars = CopyVars {
            hero: story.hero_id.and_then(bundled_hero_name).map(str::to_owned),
            ..CopyVars::default()
        };
        let mut copy = story_copy(story, &vars, seed);
        // Selection keeps the best story first, so the first of each wins.
        if story.direction == Direction::Popoff && best_popoff.is_none() {
            best_popoff = Some(format!("{}|{}", copy.value, copy.label));
            badge = Some(copy.credit.clone());
        }
        if story.direction == Direction::Roast && best_roast.is_none() {
            best_roast = Some(format!("{}|{}", copy.value, copy.label));
        }
        if evidence.len() < PERSONA_EVIDENCE {
            evidence.push((copy.value.clone(), copy.label.clone()));
        }
        let mut slide = base_slide(
            "story_distinct",
            theme_title(story.theme),
            username,
            year_label,
        );
        // Every story about a hero wears that hero's render.
        slide.art_png = story.hero_id.and_then(|hero| hero_art(art, hero));
        copy.treatment = treat(&mut slide, story, copy.treatment, rows, target, gold);
        set_beat(&mut slide, story.direction, copy);
        cards.push((story.direction, slide));
    }
    DistinctDeck {
        stories: alternate(cards),
        persona: (!selected.is_empty()).then(|| {
            let (title, subtitle) = persona_title(&selected, seed);
            Persona {
                title: title.to_owned(),
                subtitle: subtitle.to_owned(),
                evidence,
            }
        }),
        best_popoff,
        best_roast,
        badge,
        games: selected.iter().filter_map(|story| story.match_id).collect(),
        metrics: selected.iter().map(|story| story.metric).collect(),
    }
}

/// Best first, but switching between roasts and popoffs whenever the other
/// kind still has a card, so the deck never runs four jokes of one kind.
fn alternate<T>(cards: Vec<(Direction, T)>) -> Vec<T> {
    let mut want = cards.first().map(|(direction, _)| *direction);
    let (mut roasts, mut popoffs): (VecDeque<_>, VecDeque<_>) = cards
        .into_iter()
        .partition(|(direction, _)| *direction == Direction::Roast);
    let mut ordered = Vec::with_capacity(roasts.len() + popoffs.len());
    while let Some(direction) = want {
        let (mine, other) = match direction {
            Direction::Roast => (&mut roasts, &mut popoffs),
            Direction::Popoff => (&mut popoffs, &mut roasts),
        };
        let Some((taken, card)) = mine.pop_front().or_else(|| other.pop_front()) else {
            break;
        };
        ordered.push(card);
        want = Some(match taken {
            Direction::Roast => Direction::Popoff,
            Direction::Popoff => Direction::Roast,
        });
    }
    ordered
}

/// Feeds a treatment the data it draws, or falls back to `Plain` when that
/// data is missing.
fn treat(
    slide: &mut WrappedSlideData,
    story: &DistinctStory,
    treatment: Punch,
    rows: &[DistinctMatch],
    target: i64,
    gold: &dyn Fn(i64) -> Option<Vec<f64>>,
) -> Punch {
    match treatment {
        Punch::GoldGraph => {
            let lead = story
                .match_id
                .and_then(|match_id| team_gold(rows, target, match_id, gold));
            match lead {
                Some(series) => {
                    slide.series = series;
                    treatment
                }
                None => Punch::Plain,
            }
        }
        // Only upset wins are calculated; their value is one minus the odds.
        Punch::Calculated => {
            slide.stat_value = format!("{:.0}", ((1.0 - story.value) * 100.0).round());
            treatment
        }
        Punch::LossTiles => {
            slide.series = vec![1.0; story.value.max(0.0) as usize];
            treatment
        }
        // The rank bar puts the player at the very bottom; only a real rank
        // earns that.
        Punch::SkillIssue if story.rank.is_none() => Punch::Plain,
        _ => treatment,
    }
}

/// The match's team gold lead from the player's side, positive while their
/// team was ahead. `None` when the side or the parse is unknown.
fn team_gold(
    rows: &[DistinctMatch],
    target: i64,
    match_id: i64,
    gold: &dyn Fn(i64) -> Option<Vec<f64>>,
) -> Option<Vec<f64>> {
    let side = rows
        .iter()
        .find(|row| row.discord_id == target && row.match_id == match_id)?
        .side
        .as_deref()?;
    let sign = match side {
        "radiant" => 1.0,
        "dire" => -1.0,
        _ => return None,
    };
    let series = gold(match_id)?
        .into_iter()
        .map(|lead| lead * sign)
        .collect::<Vec<_>>();
    (series.iter().filter(|lead| lead.is_finite()).count() >= 2).then_some(series)
}

/// The hero's full render, or its portrait when no render is cached.
pub(super) fn hero_art(art: &ArtSources, hero: i64) -> Option<Vec<u8>> {
    art.hero_render(hero).or_else(|| art.hero_portrait(hero))
}

pub(super) fn set_beat(slide: &mut WrappedSlideData, direction: Direction, copy: StoryCopy) {
    slide.layout = WrappedLayout::TwoBeat;
    slide.theme = match direction {
        Direction::Roast => WrappedTheme::BeatRoast,
        Direction::Popoff => WrappedTheme::BeatPopoff,
    };
    slide.beat = Some(WrappedBeat {
        setup: copy.setup,
        value: copy.value,
        setup_label: copy.label,
        punch: copy.punch,
        credit: copy.credit,
        treatment: media_punch(copy.treatment),
    });
}

/// `Punch` and `WrappedPunch` live in crates that cannot see each other.
fn media_punch(punch: Punch) -> WrappedPunch {
    match punch {
        Punch::Plain => WrappedPunch::Plain,
        Punch::Cinema => WrappedPunch::Cinema,
        Punch::GoldGraph => WrappedPunch::GoldGraph,
        Punch::Stonks => WrappedPunch::Stonks,
        Punch::NotStonks => WrappedPunch::NotStonks,
        Punch::Calculated => WrappedPunch::Calculated,
        Punch::TaskFailed => WrappedPunch::TaskFailed,
        Punch::LossTiles => WrappedPunch::LossTiles,
        Punch::PressF => WrappedPunch::PressF,
        Punch::SkillIssue => WrappedPunch::SkillIssue,
        Punch::Clock => WrappedPunch::Clock,
    }
}

const fn theme_title(theme: Theme) -> &'static str {
    match theme {
        // "Your heroes" is the top-heroes card's title.
        Theme::Heroes => "Hero pool",
        Theme::Combat => "Combat",
        Theme::Farm => "Farm",
        Theme::Map => "Map control",
        Theme::Chaos => "Chaos",
        Theme::Clutch => "Clutch",
        Theme::Time => "Late nights",
        Theme::Items => "Shopping",
    }
}

/// This year's people, a teammate and an opponent first: duo, nemesis,
/// cursed duo, punching bag. `names` returns a readable display name or
/// `None`, never an id or a placeholder.
pub(super) fn people_slides(
    year: &PeopleYear,
    names: &dyn Fn(i64) -> Option<String>,
    seed: u64,
    username: &str,
    year_label: &str,
) -> Vec<WrappedSlideData> {
    [
        ("duo", "Your duo", Direction::Popoff, &year.duo),
        ("nemesis", "Your nemesis", Direction::Roast, &year.nemesis),
        (
            "cursed_duo",
            "Cursed duo",
            Direction::Roast,
            &year.cursed_duo,
        ),
        (
            "punching_bag",
            "Punching bag",
            Direction::Popoff,
            &year.punching_bag,
        ),
    ]
    .into_iter()
    .filter_map(|(kind, title, direction, pair)| {
        let pair = pair.as_ref()?;
        let copy = people_copy(kind, pair, names(pair.discord_id).as_deref(), seed);
        let mut slide = base_slide("story_people", title, username, year_label);
        set_beat(&mut slide, direction, copy);
        Some(slide)
    })
    .collect()
}

/// The gamba profit curve as a Stonks or Not Stonks card. The curve starts at
/// zero, so its end says which way the year went. A break-even year's flat
/// line is true to neither meme, so it moves to More stats.
pub(super) fn stonks(slide: &mut WrappedSlideData, gamba: &GambaYear, seed: u64) {
    let (Some(start), Some(end)) = (slide.series.first(), slide.series.last()) else {
        return;
    };
    let (key, direction, punch) = if end > start {
        ("gamba_up", Direction::Popoff, Punch::Stonks)
    } else if end < start {
        ("gamba_down", Direction::Roast, Punch::NotStonks)
    } else {
        slide.kind = super::deck::to_more_stats(&slide.kind);
        return;
    };
    let beat = side_game_copy(key, side_tier(gamba.rank), seed);
    let copy = StoryCopy {
        setup: beat.setup.to_owned(),
        value: slide.stat_value.clone(),
        label: slide.stat_label.clone(),
        punch: beat.punch.to_owned(),
        credit: format!("{} {}", slide.stat_value, slide.stat_label),
        treatment: punch,
    };
    set_beat(slide, direction, copy);
}

/// A Press F card for a year of collapsing tunnels.
pub(super) fn cave_in_slide(
    dig: &DigYear,
    seed: u64,
    username: &str,
    year_label: &str,
) -> Option<WrappedSlideData> {
    if dig.cave_ins < PRESS_F_CAVE_INS {
        return None;
    }
    let beat = side_game_copy("cave_in", side_tier(dig.rank), seed);
    let count = format_grouped(dig.cave_ins);
    let copy = StoryCopy {
        setup: beat.setup.to_owned(),
        value: count.clone(),
        label: "cave-ins".to_owned(),
        punch: beat.punch.to_owned(),
        credit: format!("{count} cave-ins this year"),
        treatment: Punch::PressF,
    };
    let mut slide = base_slide("story_dig_cave_in", "Cave-ins", username, year_label);
    set_beat(&mut slide, Direction::Roast, copy);
    Some(slide)
}

#[cfg(all(test, feature = "runtime-test-core"))]
mod tests {
    use cama_domain::wrapped_activity::ServerRank;
    use cama_domain::wrapped_distinct::Tier;

    use super::*;

    fn story(metric: &'static str, value: f64) -> DistinctStory {
        DistinctStory {
            metric,
            theme: Theme::Chaos,
            direction: Direction::Roast,
            tier: Tier::Notable,
            value,
            display: String::new(),
            rank: None,
            hero_id: None,
            match_id: Some(6),
            item: None,
            score: 0.0,
        }
    }

    fn row(side: Option<&str>) -> DistinctMatch {
        DistinctMatch {
            match_id: 6,
            discord_id: 7,
            side: side.map(str::to_owned),
            ..DistinctMatch::default()
        }
    }

    fn treated(
        story: &DistinctStory,
        punch: Punch,
        side: Option<&str>,
        gold: &dyn Fn(i64) -> Option<Vec<f64>>,
    ) -> (Punch, WrappedSlideData) {
        let mut slide = WrappedSlideData::new("story_distinct", "Chaos");
        let punch = treat(&mut slide, story, punch, &[row(side)], 7, gold);
        (punch, slide)
    }

    #[test]
    fn treatments_get_their_data_or_fall_back_to_plain() {
        let none = |_: i64| None;
        // An upset win's value is one minus its odds going in.
        let (punch, slide) = treated(&story("upset_win", 0.82), Punch::Calculated, None, &none);
        assert_eq!(punch, Punch::Calculated);
        assert_eq!(slide.stat_value, "18");
        let (punch, slide) = treated(&story("loss_streak", 4.0), Punch::LossTiles, None, &none);
        assert_eq!(punch, Punch::LossTiles);
        assert_eq!(slide.series.len(), 4);

        // Gold is radiant-oriented; a dire player's lead is its negative.
        let lead = |match_id: i64| (match_id == 6).then(|| vec![0.0, -1_000.0, -15_000.0, 3_000.0]);
        let (punch, slide) = treated(
            &story("throw", 15_000.0),
            Punch::GoldGraph,
            Some("dire"),
            &lead,
        );
        assert_eq!(punch, Punch::GoldGraph);
        assert_eq!(slide.series, [0.0, 1_000.0, 15_000.0, -3_000.0]);
        let (_, slide) = treated(
            &story("throw", 15_000.0),
            Punch::GoldGraph,
            Some("radiant"),
            &lead,
        );
        assert_eq!(slide.series, [0.0, -1_000.0, -15_000.0, 3_000.0]);
        // No parse, no side, or no line to draw: the punchline stands alone.
        for (side, gold) in [
            (Some("dire"), &none as &dyn Fn(i64) -> Option<Vec<f64>>),
            (None, &lead),
            (Some("spectator"), &lead),
            (Some("dire"), &|_: i64| Some(vec![12.0])),
        ] {
            let (punch, slide) = treated(&story("throw", 15_000.0), Punch::GoldGraph, side, gold);
            assert_eq!(punch, Punch::Plain);
            assert!(slide.series.is_empty());
        }

        // The bottom-of-the-ladder bar needs a real rank behind it.
        let unranked = story("deaths_per_game", 9.0);
        assert_eq!(
            treated(&unranked, Punch::SkillIssue, None, &none).0,
            Punch::Plain
        );
        let ranked = DistinctStory {
            rank: Some(ServerRank { rank: 1, of: 40 }),
            ..unranked
        };
        assert_eq!(
            treated(&ranked, Punch::SkillIssue, None, &none).0,
            Punch::SkillIssue
        );
    }

    #[test]
    fn every_story_punch_has_a_media_twin() {
        let all = [
            Punch::Plain,
            Punch::Cinema,
            Punch::GoldGraph,
            Punch::Stonks,
            Punch::NotStonks,
            Punch::Calculated,
            Punch::TaskFailed,
            Punch::LossTiles,
            Punch::PressF,
            Punch::SkillIssue,
            Punch::Clock,
        ];
        let mapped = all.map(media_punch);
        assert_eq!(mapped, WrappedPunch::ALL);
    }

    fn curve(series: &[f64]) -> WrappedSlideData {
        let mut slide = WrappedSlideData::new("chart_gamba", "Gamba");
        slide.series = series.to_vec();
        slide.stat_value = "-20 JC".to_owned();
        slide.stat_label = "across 3 gambles".to_owned();
        slide
    }

    #[test]
    fn stonks_follows_the_curve_and_skips_a_break_even_year() {
        let gamba = GambaYear::default();
        let mut down = curve(&[0.0, 50.0, -20.0]);
        stonks(&mut down, &gamba, 7);
        assert_eq!(down.layout, WrappedLayout::TwoBeat);
        assert_eq!(down.theme, WrappedTheme::BeatRoast);
        let beat = down.beat.expect("Not Stonks copy");
        assert_eq!(beat.treatment, WrappedPunch::NotStonks);
        assert_eq!(beat.value, "-20 JC");
        assert_eq!(beat.credit, "-20 JC across 3 gambles");
        assert!(beat.punch.to_lowercase().contains("not stonks"));

        let mut up = curve(&[0.0, -50.0, 20.0]);
        stonks(&mut up, &gamba, 7);
        assert_eq!(up.theme, WrappedTheme::BeatPopoff);
        assert_eq!(
            up.beat.expect("Stonks copy").treatment,
            WrappedPunch::Stonks
        );

        // A flat year is neither meme; its chart moves to More stats.
        let mut flat = curve(&[0.0, 40.0, 0.0]);
        stonks(&mut flat, &gamba, 7);
        assert_eq!(flat.kind, "more_chart_gamba");
        assert_eq!(flat.layout, WrappedLayout::Curve);
        assert!(flat.beat.is_none());
        let mut empty = curve(&[]);
        let before = empty.clone();
        stonks(&mut empty, &gamba, 7);
        assert_eq!(empty, before);
    }

    #[test]
    fn stories_alternate_roasts_and_popoffs_best_first() {
        use Direction::{Popoff, Roast};
        let cards = [
            (Popoff, 1),
            (Popoff, 2),
            (Popoff, 3),
            (Roast, 4),
            (Popoff, 5),
            (Roast, 6),
        ];
        assert_eq!(alternate(cards.to_vec()), [1, 4, 2, 6, 3, 5]);
        assert_eq!(alternate(vec![(Roast, 1), (Roast, 2)]), [1, 2]);
        assert!(alternate::<u8>(Vec::new()).is_empty());
    }

    #[test]
    fn a_year_of_cave_ins_earns_a_press_f() {
        let dig = |cave_ins| DigYear {
            cave_ins,
            ..DigYear::default()
        };
        assert!(cave_in_slide(&dig(2), 7, "Player", "Cama Wrapped 2026").is_none());
        let slide = cave_in_slide(&dig(3), 7, "Player", "Cama Wrapped 2026").expect("Press F");
        assert_eq!(slide.kind, "story_dig_cave_in");
        assert_eq!(slide.layout, WrappedLayout::TwoBeat);
        let beat = slide.beat.expect("Press F copy");
        assert_eq!(beat.treatment, WrappedPunch::PressF);
        assert_eq!(beat.value, "3");
        assert_eq!(beat.credit, "3 cave-ins this year");
        assert!(beat.punch.contains('F'));
    }
}
