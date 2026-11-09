//! Pools for personal records and hero stories.
//!
//! The same rules as `lines.rs`, plus some of their own:
//! - A record game may have been won or lost, and the credit line says
//!   which, so only the fastest-win lines speak to the result.
//! - Times are how long a game lasted, never when it was queued or started.
//! - No line claims an event the data cannot show: a streak, a rune, an
//!   item bought or not, a Roshan taken in a game.
//! - The number below a setup carries its unit ("27 kills"), so the setup
//!   never says it again, and no two lines share a phrasing.
//!
//! The record's number, hero and date sit on the card around these lines,
//! so the lines land the joke about that game instead of restating it.
//! Count records (first bloods, Roshans) span many games and never name a
//! hero.

use cama_domain::wrapped_distinct::Direction;

use super::{Beat, HERO_STORY_KINDS};

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
    b("Your bloodiest game:", "A one-sided documentary."),
    b("Personal best, carnage:", "Their team took turns."),
    b("The game you went hunting:", "Five heroes. One food chain."),
    b("Off the leash:", "Few got away."),
    b("The scoreboard, your way:", "One name kept coming up."),
    b("Your loudest game:", "Respawn timers on overtime."),
];

const MOST_DEATHS: &[Beat] = &[
    b("Rough one:", "You funded their items."),
    b("Personal worst, one game:", "Alive was a brief state."),
    b("New feeding record:", "Five-star service."),
    b("Spectator mode, extended:", "Nobody asked for a sequel."),
    b("Your street value:", "Cheap, and never sold out."),
    b("This many trips home.", "Fountain miles: platinum."),
    b("Your deadliest game.", "The danger was you."),
];

const MOST_ASSISTS: &[Beat] = &[
    b("Your most helpful game:", "Your fingerprints, everywhere."),
    b("Personal best, teamwork:", "Teamfights needed you there."),
    b("Fights you showed up for:", "Plenty, by the look of it."),
    b("Playmaker of the game:", "You set up. They cashed in."),
    b("Cast: {hero}.", "Supporting role, top billing."),
    b("Sharing is caring:", "Kills were a group project."),
];

const BEST_GPM: &[Beat] = &[
    b("Your richest game:", "Net worth went vertical."),
    b("Farm, peak edition:", "Alchemist would approve."),
    b("New farming record:", "The jungle camps unionized."),
    b("Farm check:", "Passed with honors."),
    b("Personal best, income:", "Farm: industrial scale."),
    b("Gold rush, one game:", "The shop was glad to see you."),
    b("The game you got rich:", "Every last hit paid off."),
];

const MOST_DAMAGE: &[Beat] = &[
    b("Your hardest-hitting game:", "Big numbers, all game long."),
    b("Personal best, violence:", "The enemy needed bigger bars."),
    b("Your biggest beating:", "Pain, wholesale."),
    b("New pain record:", "Nothing about it was fair."),
    b("The meter's top line:", "Guess whose name."),
    b("You hit them this hard.", "In one game. Unreasonable."),
];

const MOST_WARDS: &[Beat] = &[
    b("Vision, maxed out:", "The map had no secrets."),
    b("Eyes on everything:", "The fog packed up and left."),
    b("Personal best, scouting:", "Nowhere left to hide."),
    b("You planted this many.", "A forest of eyes."),
    b("Support of the game:", "Darkness: unemployed."),
    b("Lights on:", "Day and night, all yours."),
];

const LONGEST_GAME: &[Beat] = &[
    b("Your longest game:", "The ancient asked for mercy."),
    b(
        "Longest game of your year:",
        "Neither ancient wanted to go.",
    ),
    b("The game that wouldn't end:", "Even the creeps were tired."),
    b("Your marathon match:", "Late game had a late game."),
    b("Personal record, length:", "Divine Rapier o'clock."),
    b("Endurance test:", "Everyone passed. Somehow."),
    b("Overtime, then more:", "The timer ran out of digits."),
];

const FASTEST_WIN: &[Beat] = &[
    b("Your fastest win:", "Their carry was still farming."),
    b("Fastest win of your year:", "Lanes barely got going."),
    b("Speedrun, personal best:", "World record pace. Almost."),
    b("Your quickest win:", "Short and sweet."),
    b("New speed record:", "Laning phase was the game."),
    b("Smash and grab:", "In and out. Ancient down."),
    b("The win that took no time:", "Blink and you missed it."),
];

const FIRST_BLOODS: &[Beat] = &[
    b("Who struck first? You:", "Patience was never the plan."),
    b("The first kill was yours.", "Over and over."),
    b("The opening act:", "You, then everyone else."),
    b("Early and often:", "The enemy kept a tally."),
    b("The announcer's favorite:", "Practically your catchphrase."),
    b("Quick on the trigger:", "Small talk, skipped."),
];

const ROSHANS: &[Beat] = &[
    b("The pit went quiet:", "Roshan wants a new pit."),
    b("Rosh had a bad year:", "It was a you problem."),
    b("The big guy fell:", "He takes it personally now."),
    b("Who keeps killing him?", "No contest. It's you."),
    b("Your boss-fight record:", "He dreads the respawn."),
    b("Fell to you, in all:", "Pit stops, plural."),
];

/// Hero-story kinds and their lines. The hero's record sits on the card.
pub(super) const HERO_POOLS: &[(&str, &[Beat])] = &[
    (HERO_STORY_KINDS[0], HERO_BEST),
    (HERO_STORY_KINDS[1], HERO_NEMESIS),
    (HERO_STORY_KINDS[2], SIDE_SPLIT),
];

const HERO_BEST: &[Beat] = &[
    b("Your best hero:", "Your safest bet, by far."),
    b("A pick you can trust:", "A cheat code, legally."),
    b("Your secret weapon:", "Not very secret anymore."),
    b("The hero that loves you.", "The feeling is mutual."),
    b("Comfort pick, proven.", "The numbers back it up."),
    b("Draft it. Watch.", "Results do the talking."),
    b("Your winningest hero:", "Enemy drafters, be warned."),
];

const HERO_NEMESIS: &[Beat] = &[
    b("The hero that hates you:", "Stop picking it."),
    b("Dear {hero}:", "It's not working out."),
    b("Your cursed hero:", "The enemy hopes you pick it."),
    b("A hero you keep picking.", "Loyalty, misplaced."),
    b("Your anti-comfort pick:", "Uncomfortable for everyone."),
    b("Pick {hero}?", "Hard pass, says everyone."),
    b("Your worst hero:", "Your team could tell."),
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
