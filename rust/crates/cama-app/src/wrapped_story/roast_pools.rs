//! Two-beat copy for Wrapped: a setup over the big number, then a punchline
//! with a small fact line, for distinct stories, personal records, hero
//! stories, people, and side games, plus the persona title.
//!
//! Every line is hand-written and roasts play only. Placeholders (`{n}`,
//! `{hero}`, `{name}`, `{rank}`, `{of}`, `{time}`, `{item}`) are filled from
//! the story; a line whose placeholder has no value is never picked. The
//! pools themselves live in `roast_pools/lines.rs`.

use cama_domain::discord_content::{UNKNOWN_PLAYER, readable_player_name};
use cama_domain::formatting::count_noun;
use cama_domain::wrapped_activity::ServerRank;
use cama_domain::wrapped_distinct::{
    COUNT_RECORDS, Direction, DistinctStory, HeroLine, HeroYear, PairStory, PersonalRecord, Theme,
    Tier,
};

#[path = "roast_pools/lines.rs"]
mod lines;
#[path = "roast_pools/records.rs"]
mod records;

use lines::{FALLBACK_PERSONAS, ITEM_POOLS, PEOPLE_POOLS, PERSONAS, SIDE_POOLS, STORY_POOLS};
use records::{HERO_POOLS, RECORD_POOLS};

/// One setup and its punchline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Beat {
    pub setup: &'static str,
    pub punch: &'static str,
}

/// How a card lands its punchline. Mirrors `wrapped_media::WrappedPunch`,
/// which this crate cannot depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Punch {
    #[default]
    Plain,
    Cinema,
    GoldGraph,
    Stonks,
    NotStonks,
    Calculated,
    TaskFailed,
    LossTiles,
    PressF,
    SkillIssue,
    Clock,
}

/// Readable values the caller knows about a story. Ranks come from the story
/// itself, so ties and small servers never claim one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyVars {
    pub hero: Option<String>,
    /// Already resolved through `readable_player_name`.
    pub name: Option<String>,
    /// A clock reading such as `"4:12 AM"`; night-owl stories use their own.
    pub time: Option<String>,
    /// Overrides the built-in item name.
    pub item: Option<String>,
}

/// A filled two-beat card: `setup` over the big `value` and its unit `label`,
/// then `punch` with `credit`, the fact line that lets the still stand alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoryCopy {
    pub setup: String,
    pub value: String,
    pub label: String,
    pub punch: String,
    pub credit: String,
    pub treatment: Punch,
}

pub const PEOPLE_KINDS: [&str; 4] = ["duo", "cursed_duo", "nemesis", "punching_bag"];

/// Side-game keys. `pet_lost` and `cave_in` are the Press F cards.
pub const SIDE_GAME_KEYS: [&str; 7] = [
    "gamba_up",
    "gamba_down",
    "dig",
    "cave_in",
    "pet",
    "pet_lost",
    "mafia",
];

/// Longer names would shrink a beat below its design size, so they appear
/// only in the credit line.
const MAX_INLINE_NAME: usize = 12;

/// Lines for `metric`, falling back Extreme → Strong → Notable when a tier
/// has none. Empty for an unknown metric.
#[must_use]
pub fn pool(metric: &str, direction: Direction, tier: Tier) -> &'static [Beat] {
    let order: &[Tier] = match tier {
        Tier::Extreme => &[Tier::Extreme, Tier::Strong, Tier::Notable],
        Tier::Strong => &[Tier::Strong, Tier::Notable],
        Tier::Notable => &[Tier::Notable],
    };
    order
        .iter()
        .find_map(|wanted| {
            STORY_POOLS
                .iter()
                .find(|(key, way, level, _)| *key == metric && *way == direction && level == wanted)
                .map(|(_, _, _, lines)| *lines)
        })
        .unwrap_or(&[])
}

/// The card treatment for a story or side-game key. `story_copy` also turns
/// the server's worst Combat or Farm roasts into `SkillIssue`.
#[must_use]
pub fn treatment(metric: &str, direction: Direction) -> Punch {
    match (metric, direction) {
        ("best_game", Direction::Popoff) => Punch::Cinema,
        ("throw", _) => Punch::GoldGraph,
        ("upset_win", _) => Punch::Calculated,
        ("kill_loss", _) => Punch::TaskFailed,
        ("loss_streak", _) => Punch::LossTiles,
        ("night_owl", _) => Punch::Clock,
        ("gamba_up", _) => Punch::Stonks,
        ("gamba_down", _) => Punch::NotStonks,
        ("pet_lost" | "cave_in", _) => Punch::PressF,
        _ => Punch::Plain,
    }
}

fn story_treatment(story: &DistinctStory) -> Punch {
    let base = treatment(story.metric, story.direction);
    let bottom = story.direction == Direction::Roast
        && story.tier == Tier::Extreme
        && matches!(story.theme, Theme::Combat | Theme::Farm);
    if base == Punch::Plain && bottom {
        Punch::SkillIssue
    } else {
        base
    }
}

/// The card copy for one distinct story, picked deterministically by `seed`
/// (`chapters::story_seed`).
#[must_use]
pub fn story_copy(story: &DistinctStory, vars: &CopyVars, seed: u64) -> StoryCopy {
    let item_key = story.item.as_deref().filter(|_| story.metric == "item");
    let item = vars
        .item
        .clone()
        .or_else(|| item_key.and_then(item_name).map(str::to_owned));
    let lines = match item_key.map(item_pool) {
        Some(own) if !own.is_empty() => own,
        _ => pool(story.metric, story.direction, story.tier),
    };
    let (rank, of) = story
        .rank
        .map(|rank| (rank.rank.to_string(), rank.of.to_string()))
        .unzip();
    let time = vars
        .time
        .clone()
        .or_else(|| (story.metric == "night_owl").then(|| story.display.clone()));
    let hero = vars.hero.as_deref();
    let slots = [
        ("n", Some(story.display.as_str())),
        ("hero", hero),
        ("name", inline_name(vars.name.as_deref())),
        ("rank", rank.as_deref()),
        ("of", of.as_deref()),
        ("time", time.as_deref()),
        ("item", item.as_deref()),
    ];
    let salt = mix_text(&[
        story.metric,
        item_key.unwrap_or(""),
        direction_key(story.direction),
        tier_key(story.tier),
    ]);
    let (setup, punch) = choose(lines, seed, salt, &slots).unwrap_or_default();
    let (label, mut credit) = facts(story, hero, item.as_deref().unwrap_or("item"));
    if let Some(rank) = story.rank {
        credit = format!("{credit} · {}", rank_phrase(story.direction, rank));
    }
    StoryCopy {
        setup,
        value: story.display.clone(),
        label,
        punch,
        credit,
        treatment: story_treatment(story),
    }
}

/// The unit under the big number and the punchline's fact line.
fn facts(story: &DistinctStory, hero: Option<&str>, item: &str) -> (String, String) {
    let n = story.display.as_str();
    let plural = |one: &str, many: &str| {
        if story.value == 1.0 {
            one.to_owned()
        } else {
            many.to_owned()
        }
    };
    let simple = |unit: &str| (unit.to_owned(), format!("{n} {unit}"));
    match story.metric {
        "win_rate" => simple("win rate"),
        "deaths_per_game" => simple("deaths a game"),
        "kills_per_game" => simple("kills a game"),
        "best_game" => (
            hero.unwrap_or("K / D / A").to_owned(),
            hero.map_or_else(|| n.to_owned(), |hero| format!("{n} · {hero}")),
        ),
        "kill_loss" => (
            "kills in a loss".to_owned(),
            hero.map_or_else(
                || format!("{n} kills in a loss"),
                |hero| format!("{n} kills · {hero}"),
            ),
        ),
        "gpm" => ("gold a minute".to_owned(), format!("{n} GPM")),
        "last_hits" => simple("last hits a minute"),
        "hero_damage" => simple("hero damage a minute"),
        "healing" => simple("healing a minute"),
        "stuns" => (
            "seconds of stuns a game".to_owned(),
            format!("{n}s of stuns a game"),
        ),
        "wards" => simple("wards a game"),
        "stacks" => simple("camps stacked a game"),
        "runes" => simple("runes a game"),
        "couriers" => (
            plural("courier sniped", "couriers sniped"),
            format!("{n} {} in parsed games", plural("courier", "couriers")),
        ),
        "pings" => simple("pings a game"),
        "apm" => ("actions a minute".to_owned(), format!("{n} APM")),
        "throw" => ("team gold lead".to_owned(), format!("Team gold lead +{n}")),
        "comeback" => (
            "team gold deficit".to_owned(),
            format!("Team was down {n} gold and won"),
        ),
        "rapiers" => {
            let swords = plural("Divine Rapier", "Divine Rapiers");
            let credit = if story.direction == Direction::Roast {
                format!("{n} {swords} · lost with one")
            } else {
                format!("{n} {swords} in parsed games")
            };
            (swords, credit)
        }
        "upset_win" => ("win odds going in".to_owned(), format!("Won at {n} odds")),
        "upset_loss" => ("win odds going in".to_owned(), format!("Lost at {n} odds")),
        "win_streak" => simple("wins in a row"),
        "loss_streak" => simple("losses in a row"),
        "rating_swing" => simple("rating in one game"),
        "night_owl" => (
            "latest game finish".to_owned(),
            format!(
                "{} finished after midnight",
                count_noun(story.value.round() as i64, "game", "games")
            ),
        ),
        "marathon_day" => simple("games in one day"),
        "hero_loyal" => {
            let on = hero.unwrap_or("one hero");
            (
                format!("of your games on {on}"),
                format!("{n} of games on {on}"),
            )
        }
        "hero_pool" => simple("played this year"),
        "buybacks" => simple("buybacks a game"),
        "item" => simple(&format!("{item} buys a game")),
        _ => (String::new(), n.to_owned()),
    }
}

/// Server rank from the story's own end: the best for a popoff, the worst
/// for a roast.
fn rank_phrase(direction: Direction, rank: ServerRank) -> String {
    match direction {
        Direction::Popoff => format!("#{} of {}", rank.rank, rank.of),
        Direction::Roast if rank.rank == 1 => format!("Dead last of {}", rank.of),
        Direction::Roast => format!("{} from last of {}", ordinal(rank.rank), rank.of),
    }
}

fn ordinal(value: i64) -> String {
    let suffix = match (value % 10, value % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{value}{suffix}")
}

/// Copy for a year-scoped teammate or rival. `name` is skipped unless it is
/// readable, and only short names appear inside the joke.
#[must_use]
pub fn people_copy(kind: &str, pair: &PairStory, name: Option<&str>, seed: u64) -> StoryCopy {
    let lines = people_pool(kind);
    if lines.is_empty() {
        return StoryCopy::default();
    }
    let readable = name
        .and_then(readable_player_name)
        .filter(|name| *name != UNKNOWN_PLAYER);
    let inline = inline_name(readable);
    let (wins, games) = (pair.wins, pair.games);
    let losses = games - wins;
    let with = readable.map_or_else(
        || format!("{wins} of {games} won together"),
        |name| format!("{wins} of {games} won with {name}"),
    );
    let against = format!(
        "Won {wins} of {games} against {}",
        readable.unwrap_or("them")
    );
    let them = inline.unwrap_or("them");
    let (value, label, credit) = match kind {
        "duo" => (wins.to_string(), "wins together".to_owned(), with),
        "cursed_duo" => {
            let rate = if games > 0 {
                (wins as f64 * 100.0 / games as f64).round()
            } else {
                0.0
            };
            (format!("{rate}%"), "win rate together".to_owned(), with)
        }
        "nemesis" => {
            let label = if inline.is_some() {
                format!("losses to {them}")
            } else {
                "losses against them".to_owned()
            };
            (losses.to_string(), label, against)
        }
        _ => (wins.to_string(), format!("wins against {them}"), against),
    };
    let (setup, punch) =
        choose(lines, seed, mix_text(&[kind]), &[("name", inline)]).unwrap_or_default();
    StoryCopy {
        setup,
        value,
        label,
        punch,
        credit,
        treatment: Punch::Plain,
    }
}

/// A persona title and subtitle from the player's strongest trait that has
/// one, or a general one.
#[must_use]
pub fn persona_title(top: &[DistinctStory], seed: u64) -> (&'static str, &'static str) {
    let found = top.iter().find_map(|story| {
        PERSONAS
            .iter()
            .find(|(metric, direction, _)| *metric == story.metric && *direction == story.direction)
            .map(|(metric, direction, titles)| {
                (*titles, mix_text(&[metric, direction_key(*direction)]))
            })
    });
    let (titles, salt) = found.unwrap_or((FALLBACK_PERSONAS, mix_text(&["persona"])));
    titles[index(seed, salt, titles.len())]
}

/// A headline beat for a side-game card. Unknown keys get an empty beat.
#[must_use]
pub fn side_game_copy(key: &str, tier: Tier, seed: u64) -> Beat {
    let lines = side_pool(key, tier);
    if lines.is_empty() {
        return Beat {
            setup: "",
            punch: "",
        };
    }
    lines[index(seed, mix_text(&[key, tier_key(tier)]), lines.len())]
}

/// Hero-story kinds: the best hero, the hero that hates you, and a
/// Radiant/Dire split.
pub const HERO_STORY_KINDS: [&str; 3] = ["hero_best", "hero_nemesis", "side_split"];

/// Lines for a personal record read `direction`'s way; empty for an
/// unknown key or the other way.
#[must_use]
pub fn record_pool(key: &str, direction: Direction) -> &'static [Beat] {
    RECORD_POOLS
        .iter()
        .find(|(pool, way, _)| *pool == key && *way == direction)
        .map_or(&[], |(_, _, lines)| *lines)
}

/// Joins the parts a card knows with `" · "`.
fn dotted<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> String {
    parts.into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

/// The card copy for one personal record: `hero` is the record game's
/// readable hero name and `date` its day, such as `"Mar 3"`. A count record
/// spans the year, so it names the latest game's hero but no date or result.
#[must_use]
pub fn record_copy(
    record: &PersonalRecord,
    hero: Option<&str>,
    date: Option<&str>,
    seed: u64,
) -> StoryCopy {
    let number = record.display.split(' ').next().unwrap_or_default();
    let slots = [("n", Some(number)), ("hero", hero)];
    let salt = mix_text(&["record", record.key]);
    let (setup, punch) = choose(
        record_pool(record.key, record.direction),
        seed,
        salt,
        &slots,
    )
    .unwrap_or_default();
    let (label, credit) = if COUNT_RECORDS.contains(&record.key) {
        let counted = format!("{} in parsed games", record.display);
        let latest = hero.map(|hero| format!("latest on {hero}"));
        (
            "in parsed games".to_owned(),
            dotted([Some(counted.as_str()), latest.as_deref()]),
        )
    } else {
        let result = record.won.map(|won| if won { "Won" } else { "Lost" });
        let label = dotted([hero, date]);
        (
            if label.is_empty() {
                "in one game".to_owned()
            } else {
                label
            },
            dotted([Some(record.display.as_str()), hero, date, result]),
        )
    };
    StoryCopy {
        setup,
        value: record.display.clone(),
        label,
        punch,
        credit,
        treatment: Punch::Plain,
    }
}

/// A win rate in whole percent.
fn rate(wins: i64, games: i64) -> String {
    let percent = if games > 0 {
        (wins as f64 * 100.0 / games as f64).round()
    } else {
        0.0
    };
    format!("{percent}%")
}

/// The card copy for `hero_best` or `hero_nemesis`: the hero's win rate and
/// record. Other kinds get empty copy.
#[must_use]
pub fn hero_copy(kind: &str, line: &HeroLine, hero: Option<&str>, seed: u64) -> StoryCopy {
    let [best, nemesis, _] = HERO_STORY_KINDS;
    if kind != best && kind != nemesis {
        return StoryCopy::default();
    }
    let (setup, punch) =
        choose(hero_pool(kind), seed, mix_text(&[kind]), &[("hero", hero)]).unwrap_or_default();
    let record = format!("Won {} of {}", line.wins, line.games);
    StoryCopy {
        setup,
        value: rate(line.wins, line.games),
        label: format!("win rate on {}", hero.unwrap_or("this hero")),
        punch,
        credit: match hero {
            Some(hero) => format!("{record} on {hero}"),
            None => record,
        },
        treatment: Punch::Plain,
    }
}

/// The card copy for a Radiant/Dire split, when the year has one.
#[must_use]
pub fn side_split_copy(year: &HeroYear, seed: u64) -> Option<StoryCopy> {
    let better = year.side_split()?;
    let ((radiant_games, radiant_wins), (dire_games, dire_wins)) = (year.radiant, year.dire);
    let (radiant, dire) = (
        rate(radiant_wins, radiant_games),
        rate(dire_wins, dire_games),
    );
    let (setup, punch) = choose(
        hero_pool(HERO_STORY_KINDS[2]),
        seed,
        mix_text(&[HERO_STORY_KINDS[2]]),
        &[],
    )
    .unwrap_or_default();
    Some(StoryCopy {
        setup,
        value: if better == "Radiant" {
            radiant.clone()
        } else {
            dire.clone()
        },
        label: format!("win rate as {better}"),
        punch,
        credit: format!("Radiant {radiant} · Dire {dire}"),
        treatment: Punch::Plain,
    })
}

fn hero_pool(kind: &str) -> &'static [Beat] {
    HERO_POOLS
        .iter()
        .find(|(key, _)| *key == kind)
        .map_or(&[], |(_, lines)| *lines)
}

/// Every beat in every pool, for fit and tone checks.
#[must_use]
pub fn all_beats() -> Vec<&'static Beat> {
    let story = STORY_POOLS.iter().flat_map(|(_, _, _, lines)| lines.iter());
    let items = ITEM_POOLS.iter().flat_map(|(_, _, lines)| lines.iter());
    let people = PEOPLE_POOLS.iter().flat_map(|(_, lines)| lines.iter());
    let side = SIDE_POOLS.iter().flat_map(|(_, _, lines)| lines.iter());
    let records = RECORD_POOLS.iter().flat_map(|(_, _, lines)| lines.iter());
    let heroes = HERO_POOLS.iter().flat_map(|(_, lines)| lines.iter());
    story
        .chain(items)
        .chain(people)
        .chain(side)
        .chain(records)
        .chain(heroes)
        .collect()
}

fn item_pool(item: &str) -> &'static [Beat] {
    ITEM_POOLS
        .iter()
        .find(|(key, _, _)| *key == item)
        .map_or(&[], |(_, _, lines)| *lines)
}

fn item_name(item: &str) -> Option<&'static str> {
    ITEM_POOLS
        .iter()
        .find(|(key, _, _)| *key == item)
        .map(|(_, name, _)| *name)
}

fn people_pool(kind: &str) -> &'static [Beat] {
    PEOPLE_POOLS
        .iter()
        .find(|(key, _)| *key == kind)
        .map_or(&[], |(_, lines)| *lines)
}

fn side_pool(key: &str, tier: Tier) -> &'static [Beat] {
    let order: &[Tier] = match tier {
        Tier::Extreme => &[Tier::Extreme, Tier::Strong, Tier::Notable],
        Tier::Strong => &[Tier::Strong, Tier::Notable],
        Tier::Notable => &[Tier::Notable],
    };
    order
        .iter()
        .find_map(|wanted| {
            SIDE_POOLS
                .iter()
                .find(|(side, level, _)| *side == key && level == wanted)
                .map(|(_, _, lines)| *lines)
        })
        .unwrap_or(&[])
}

fn inline_name(name: Option<&str>) -> Option<&str> {
    name.filter(|name| name.chars().count() <= MAX_INLINE_NAME)
}

/// The filled setup and punch of a line whose placeholders all have values,
/// picked by `seed`.
fn choose(
    lines: &[Beat],
    seed: u64,
    salt: u64,
    slots: &[(&str, Option<&str>)],
) -> Option<(String, String)> {
    let usable: Vec<(String, String)> = lines
        .iter()
        .filter_map(|beat| Some((fill(beat.setup, slots)?, fill(beat.punch, slots)?)))
        .collect();
    if usable.is_empty() {
        return None;
    }
    let picked = index(seed, salt, usable.len());
    usable.into_iter().nth(picked)
}

/// `template` with every `{slot}` replaced, or `None` when a slot is unknown
/// or has no value.
fn fill(template: &str, slots: &[(&str, Option<&str>)]) -> Option<String> {
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = start + rest[start..].find('}')?;
        let key = &rest[start + 1..end];
        let value = slots.iter().find(|(slot, _)| *slot == key)?.1?;
        filled.push_str(&rest[..start]);
        filled.push_str(value);
        rest = &rest[end + 1..];
    }
    filled.push_str(rest);
    Some(filled)
}

/// A well-spread index below `len` for this seed and pool.
fn index(seed: u64, salt: u64, len: usize) -> usize {
    let mut mixed = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    mixed ^= mixed >> 33;
    (mixed % len.max(1) as u64) as usize
}

/// FNV-1a over `parts`, so each pool picks independently of the others.
fn mix_text(parts: &[&str]) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325_u64;
    for part in parts {
        for byte in part.bytes().chain([0xFF]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01B3);
        }
    }
    hash
}

fn direction_key(direction: Direction) -> &'static str {
    match direction {
        Direction::Roast => "roast",
        Direction::Popoff => "popoff",
    }
}

fn tier_key(tier: Tier) -> &'static str {
    match tier {
        Tier::Extreme => "extreme",
        Tier::Strong => "strong",
        Tier::Notable => "notable",
    }
}

#[cfg(test)]
#[path = "roast_pools/tests.rs"]
mod tests;
