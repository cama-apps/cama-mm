//! Pools for personal records and hero stories.
//!
//! The same rules as `lines.rs`, plus two of their own:
//! - A record game may have been won or lost, and the credit line says
//!   which, so only the fastest-win lines speak to the result.
//! - Times are how long a game lasted, never when it was queued or started.
//!
//! The record's number, hero and date sit on the card around these lines,
//! so the lines land the joke about that game instead of restating it.
//! Count records (first bloods, Roshans) span many games and never name a
//! hero.

use cama_domain::wrapped_distinct::Direction;

use super::Beat;

use Direction::{Popoff, Roast};

const fn b(setup: &'static str, punch: &'static str) -> Beat {
    Beat { setup, punch }
}

pub(super) const RECORD_POOLS: &[(&str, Direction, &[Beat])] = &[
    ("most_kills", Popoff, MOST_KILLS),
    ("most_deaths", Roast, MOST_DEATHS),
    ("most_assists", Popoff, MOST_ASSISTS),
    ("best_gpm", Popoff, BEST_GPM),
    ("most_damage", Popoff, MOST_DAMAGE),
    ("most_wards", Popoff, MOST_WARDS),
    ("longest_game", Roast, LONGEST_GAME),
    ("fastest_win", Popoff, FASTEST_WIN),
    ("first_bloods", Popoff, FIRST_BLOODS),
    ("roshans", Popoff, ROSHANS),
];

const MOST_KILLS: &[Beat] = &[
    b("Your most kills in a game:", "Beyond godlike. Way beyond."),
    b("Personal best, kills:", "The enemy team took turns."),
    b("The game you went hunting:", "Five heroes. One food chain."),
    b("Peak {hero}.", "Their whole team saw grey."),
    b("New kill record:", "Kill feed: you, on repeat."),
    b("Your bloodiest game:", "Their respawn timers felt it."),
    b("You and {hero}:", "Their fountain stayed busy."),
];

const MOST_DEATHS: &[Beat] = &[
    b(
        "Times you died in one game:",
        "The fountain knew you by name.",
    ),
    b("Personal worst, deaths:", "Their carry sent a thank-you."),
    b("New feeding record:", "You were their jungle camp."),
    b(
        "Your most deaths in a game:",
        "Grey screen: director's cut.",
    ),
    b("Death count, one game:", "Worth more than a bounty rune."),
    b("You and {hero}:", "Mostly a respawn timer."),
    b("Your deadliest game.", "For you, specifically."),
];

const MOST_ASSISTS: &[Beat] = &[
    b("Your most assists in a game:", "Your name on every kill."),
    b("Personal best, assists:", "Teamfight participation: yes."),
    b("You were in every fight.", "Even the ones you didn't pick."),
    b("New assist record:", "You set up. They cashed in."),
    b("Peak {hero}.", "Present at every kill."),
    b("Your most generous game:", "Kills were a team sport."),
];

const BEST_GPM: &[Beat] = &[
    b("Your richest game:", "The jungle had no secrets."),
    b("Gold per minute, peak:", "Alchemist took notes."),
    b("New farming record:", "The jungle camps unionized."),
    b("Peak {hero}.", "Every camp. Every wave."),
    b("Personal best, gold:", "Hand of Midas got jealous."),
    b("Your best GPM game:", "No creep wave survived."),
    b("The game you got rich:", "Net worth: disrespectful."),
];

const MOST_DAMAGE: &[Beat] = &[
    b("Your hardest-hitting game:", "Their salves ran out."),
    b(
        "Most hero damage this year:",
        "The enemy needed bigger bars.",
    ),
    b("Personal best, damage:", "Someone had to absorb that."),
    b("New damage record:", "Health bars were a suggestion."),
    b("Peak {hero}.", "Their whole team felt it."),
    b("You hit them this hard.", "In one game. Unreasonable."),
];

const MOST_WARDS: &[Beat] = &[
    b("Most wards in a game:", "The map had no secrets."),
    b("Your warding record:", "Every cliff. Every rune."),
    b("Personal best, vision:", "Their smokes went nowhere."),
    b("You planted this many.", "A forest of eyes."),
    b("New ward record:", "Their jungle felt watched."),
    b("Peak {hero}.", "The map lit up like day."),
    b("The game you lit the map:", "Dust, sentries, the works."),
];

const LONGEST_GAME: &[Beat] = &[
    b("Your longest game:", "The ancient asked for mercy."),
    b("Longest game of your year:", "Six slots full. Still going."),
    b("The game that wouldn't end:", "Even the creeps were tired."),
    b("Your marathon match:", "Buyback, buyback, buyback."),
    b("Personal record, length:", "Divine Rapier o'clock."),
    b("Peak {hero}.", "Peak endurance, anyway."),
    b("Longest game you played:", "Roshan respawned out of spite."),
];

const FASTEST_WIN: &[Beat] = &[
    b("Your fastest win:", "Their carry was still farming."),
    b("Fastest win of your year:", "The ancient barely loaded in."),
    b("Speedrun, personal best:", "Any percent, no Roshan."),
    b("Your quickest win:", "GG, go next. Quickly."),
    b("New speed record:", "The creeps barely met."),
    b("Peak {hero}.", "In and out. Ancient down."),
    b(
        "The win that took no time:",
        "Their BKB never left the shop.",
    ),
];

const FIRST_BLOODS: &[Beat] = &[
    b("Who struck first? You:", "Level one, already hunting."),
    b("You drew first blood.", "This many times."),
    b("First blood, again:", "The announcer knows your name."),
    b("Opening kills, all yours:", "Laning phase? Kill phase."),
    b("The first kill was yours.", "Over and over."),
    b("First blood, on repeat:", "Patience was never the plan."),
];

const ROSHANS: &[Beat] = &[
    b("The pit went quiet:", "Aegis enjoyer, certified."),
    b("Rosh had a bad year:", "Cheese for everyone."),
    b("You took Roshan.", "Repeatedly. He noticed."),
    b("Roshan's least favorite:", "You. Specifically you."),
    b("The pit was yours.", "Roshan dreads the respawn."),
    b("Roshans that fell to you:", "The Aegis knows you by now."),
];

/// Hero-story kinds and their lines. The hero's record sits on the card.
pub(super) const HERO_POOLS: &[(&str, &[Beat])] = &[
    ("hero_best", HERO_BEST),
    ("hero_nemesis", HERO_NEMESIS),
    ("side_split", SIDE_SPLIT),
];

const HERO_BEST: &[Beat] = &[
    b("Your best hero:", "Pick it. Win. Repeat."),
    b("You and {hero}:", "Basically a cheat code."),
    b("Your secret weapon:", "Not very secret anymore."),
    b("The hero that loves you.", "The feeling is mutual."),
    b("Comfort pick, proven.", "The numbers back it up."),
    b("Ban {hero}.", "Or lose to it. Your call."),
    b("Your winningest hero:", "Enemy drafters, take note."),
];

const HERO_NEMESIS: &[Beat] = &[
    b("The hero that hates you:", "Stop picking it."),
    b("You and {hero}:", "It's not working out."),
    b("Your cursed hero:", "The enemy hopes you pick it."),
    b("A hero you keep picking.", "It doesn't love you back."),
    b("Your anti-comfort pick:", "Uncomfortable for everyone."),
    b("Pick {hero}?", "Lose. Repeat."),
    b("Your worst hero:", "Your team saw it coming."),
];

/// A side split names no side in its lines; the card shows both.
const SIDE_SPLIT: &[Beat] = &[
    b("Radiant or Dire?", "It matters. A lot."),
    b("One side of the river:", "You're a different player."),
    b("Same player. Other side.", "Different results."),
    b("The map has two sides.", "You only mastered one."),
    b("Swap sides, swap skill.", "The river divides you."),
    b("Pick a side. Any side.", "Not that one, though."),
];
