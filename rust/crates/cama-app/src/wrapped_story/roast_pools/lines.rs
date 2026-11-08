//! The hand-written pools behind `roast_pools`.
//!
//! Rules every line follows (the tests enforce the measurable ones):
//! - Roast play only: Dota, gamba, Dig, pets, Mafia. Never looks,
//!   intelligence, identity, family, relationships, jobs, or life outside
//!   the game. How much and how late someone plays is fair game.
//! - Setups stay within 28 characters and punchlines within 30 after the
//!   widest placeholder values. The big number sits in its own slot.
//! - Meme cards keep the meme's phrase as the punchline; their variety lives
//!   in the setups.
//! - Truthful: throws and comebacks are team gold, night-owl times are when
//!   a game finished, Extreme lines never claim #1 without `{rank}`.

use cama_domain::wrapped_distinct::{Direction, Tier};

use super::Beat;

use Direction::{Popoff, Roast};
use Tier::{Extreme, Notable, Strong};

const fn b(setup: &'static str, punch: &'static str) -> Beat {
    Beat { setup, punch }
}

pub(super) const STORY_POOLS: &[(&str, Direction, Tier, &[Beat])] = &[
    ("win_rate", Popoff, Extreme, WIN_RATE_POPOFF_EXTREME),
    ("win_rate", Popoff, Strong, WIN_RATE_POPOFF_STRONG),
    ("win_rate", Popoff, Notable, WIN_RATE_POPOFF_NOTABLE),
    ("win_rate", Roast, Extreme, WIN_RATE_ROAST_EXTREME),
    ("win_rate", Roast, Strong, WIN_RATE_ROAST_STRONG),
    ("win_rate", Roast, Notable, WIN_RATE_ROAST_NOTABLE),
    ("deaths_per_game", Roast, Extreme, DEATHS_ROAST_EXTREME),
    ("deaths_per_game", Roast, Strong, DEATHS_ROAST_STRONG),
    ("deaths_per_game", Roast, Notable, DEATHS_ROAST_NOTABLE),
    ("deaths_per_game", Popoff, Extreme, DEATHS_POPOFF_EXTREME),
    ("deaths_per_game", Popoff, Strong, DEATHS_POPOFF_STRONG),
    ("deaths_per_game", Popoff, Notable, DEATHS_POPOFF_NOTABLE),
    ("kills_per_game", Popoff, Extreme, KILLS_EXTREME),
    ("kills_per_game", Popoff, Strong, KILLS_STRONG),
    ("kills_per_game", Popoff, Notable, KILLS_NOTABLE),
    ("best_game", Popoff, Extreme, BEST_GAME_EXTREME),
    ("best_game", Popoff, Strong, BEST_GAME_STRONG),
    ("best_game", Popoff, Notable, BEST_GAME_NOTABLE),
    ("kill_loss", Roast, Extreme, KILL_LOSS_EXTREME),
    ("kill_loss", Roast, Strong, KILL_LOSS_STRONG),
    ("kill_loss", Roast, Notable, KILL_LOSS_NOTABLE),
    ("gpm", Popoff, Extreme, GPM_POPOFF_EXTREME),
    ("gpm", Popoff, Strong, GPM_POPOFF_STRONG),
    ("gpm", Popoff, Notable, GPM_POPOFF_NOTABLE),
    ("gpm", Roast, Extreme, GPM_ROAST_EXTREME),
    ("gpm", Roast, Strong, GPM_ROAST_STRONG),
    ("gpm", Roast, Notable, GPM_ROAST_NOTABLE),
    ("last_hits", Popoff, Extreme, LAST_HITS_EXTREME),
    ("last_hits", Popoff, Strong, LAST_HITS_STRONG),
    ("last_hits", Popoff, Notable, LAST_HITS_NOTABLE),
    ("hero_damage", Popoff, Extreme, HERO_DAMAGE_EXTREME),
    ("hero_damage", Popoff, Strong, HERO_DAMAGE_STRONG),
    ("hero_damage", Popoff, Notable, HERO_DAMAGE_NOTABLE),
    ("healing", Popoff, Extreme, HEALING_EXTREME),
    ("healing", Popoff, Strong, HEALING_STRONG),
    ("healing", Popoff, Notable, HEALING_NOTABLE),
    ("stuns", Popoff, Extreme, STUNS_EXTREME),
    ("stuns", Popoff, Strong, STUNS_STRONG),
    ("stuns", Popoff, Notable, STUNS_NOTABLE),
    ("wards", Popoff, Extreme, WARDS_POPOFF_EXTREME),
    ("wards", Popoff, Strong, WARDS_POPOFF_STRONG),
    ("wards", Popoff, Notable, WARDS_POPOFF_NOTABLE),
    ("wards", Roast, Extreme, WARDS_ROAST_EXTREME),
    ("wards", Roast, Strong, WARDS_ROAST_STRONG),
    ("wards", Roast, Notable, WARDS_ROAST_NOTABLE),
    ("stacks", Popoff, Extreme, STACKS_EXTREME),
    ("stacks", Popoff, Strong, STACKS_STRONG),
    ("stacks", Popoff, Notable, STACKS_NOTABLE),
    ("runes", Popoff, Extreme, RUNES_EXTREME),
    ("runes", Popoff, Strong, RUNES_STRONG),
    ("runes", Popoff, Notable, RUNES_NOTABLE),
    ("couriers", Popoff, Extreme, COURIERS_EXTREME),
    ("couriers", Popoff, Strong, COURIERS_STRONG),
    ("couriers", Popoff, Notable, COURIERS_NOTABLE),
    ("pings", Roast, Extreme, PINGS_EXTREME),
    ("pings", Roast, Strong, PINGS_STRONG),
    ("pings", Roast, Notable, PINGS_NOTABLE),
    ("apm", Popoff, Extreme, APM_EXTREME),
    ("apm", Popoff, Strong, APM_STRONG),
    ("apm", Popoff, Notable, APM_NOTABLE),
    ("throw", Roast, Extreme, THROW_EXTREME),
    ("throw", Roast, Strong, THROW_STRONG),
    ("throw", Roast, Notable, THROW_NOTABLE),
    ("comeback", Popoff, Extreme, COMEBACK_EXTREME),
    ("comeback", Popoff, Strong, COMEBACK_STRONG),
    ("comeback", Popoff, Notable, COMEBACK_NOTABLE),
    ("rapiers", Popoff, Extreme, RAPIERS_POPOFF_EXTREME),
    ("rapiers", Popoff, Strong, RAPIERS_POPOFF_STRONG),
    ("rapiers", Popoff, Notable, RAPIERS_POPOFF_NOTABLE),
    ("rapiers", Roast, Extreme, RAPIERS_ROAST_EXTREME),
    ("rapiers", Roast, Strong, RAPIERS_ROAST_STRONG),
    ("rapiers", Roast, Notable, RAPIERS_ROAST_NOTABLE),
    ("upset_win", Popoff, Extreme, UPSET_WIN_EXTREME),
    ("upset_win", Popoff, Strong, UPSET_WIN_STRONG),
    ("upset_win", Popoff, Notable, UPSET_WIN_NOTABLE),
    ("upset_loss", Roast, Extreme, UPSET_LOSS_EXTREME),
    ("upset_loss", Roast, Strong, UPSET_LOSS_STRONG),
    ("upset_loss", Roast, Notable, UPSET_LOSS_NOTABLE),
    ("win_streak", Popoff, Extreme, WIN_STREAK_EXTREME),
    ("win_streak", Popoff, Strong, WIN_STREAK_STRONG),
    ("win_streak", Popoff, Notable, WIN_STREAK_NOTABLE),
    ("loss_streak", Roast, Extreme, LOSS_STREAK_EXTREME),
    ("loss_streak", Roast, Strong, LOSS_STREAK_STRONG),
    ("loss_streak", Roast, Notable, LOSS_STREAK_NOTABLE),
    ("rating_swing", Popoff, Extreme, RATING_POPOFF_EXTREME),
    ("rating_swing", Popoff, Strong, RATING_POPOFF_STRONG),
    ("rating_swing", Popoff, Notable, RATING_POPOFF_NOTABLE),
    ("rating_swing", Roast, Extreme, RATING_ROAST_EXTREME),
    ("rating_swing", Roast, Strong, RATING_ROAST_STRONG),
    ("rating_swing", Roast, Notable, RATING_ROAST_NOTABLE),
    ("night_owl", Roast, Extreme, NIGHT_OWL_EXTREME),
    ("night_owl", Roast, Strong, NIGHT_OWL_STRONG),
    ("night_owl", Roast, Notable, NIGHT_OWL_NOTABLE),
    ("marathon_day", Roast, Extreme, MARATHON_EXTREME),
    ("marathon_day", Roast, Strong, MARATHON_STRONG),
    ("marathon_day", Roast, Notable, MARATHON_NOTABLE),
    ("hero_loyal", Popoff, Extreme, LOYAL_POPOFF_EXTREME),
    ("hero_loyal", Popoff, Strong, LOYAL_POPOFF_STRONG),
    ("hero_loyal", Popoff, Notable, LOYAL_POPOFF_NOTABLE),
    ("hero_loyal", Roast, Extreme, LOYAL_ROAST_EXTREME),
    ("hero_loyal", Roast, Strong, LOYAL_ROAST_STRONG),
    ("hero_loyal", Roast, Notable, LOYAL_ROAST_NOTABLE),
    ("hero_pool", Popoff, Extreme, HERO_POOL_POPOFF_EXTREME),
    ("hero_pool", Popoff, Strong, HERO_POOL_POPOFF_STRONG),
    ("hero_pool", Popoff, Notable, HERO_POOL_POPOFF_NOTABLE),
    ("hero_pool", Roast, Extreme, HERO_POOL_ROAST_EXTREME),
    ("hero_pool", Roast, Strong, HERO_POOL_ROAST_STRONG),
    ("hero_pool", Roast, Notable, HERO_POOL_ROAST_NOTABLE),
    ("buybacks", Roast, Extreme, BUYBACKS_EXTREME),
    ("buybacks", Roast, Strong, BUYBACKS_STRONG),
    ("buybacks", Roast, Notable, BUYBACKS_NOTABLE),
    // Every tracked item has its own pool; this covers any other.
    ("item", Roast, Notable, ITEM_ANY),
];

// Clutch: win rate.

const WIN_RATE_POPOFF_EXTREME: &[Beat] = &[
    b("Balanced shuffle, they said.", "You were the imbalance."),
    b("Win rate rank: #{rank} of {of}", "The team diff was you."),
    b("Smurf check: inconclusive.", "Main account. Allegedly."),
    b("They saw you in the lobby.", "Defensive picks, every time."),
    b("The matchmaker tried.", "It cannot contain you."),
    b(
        "Built different, confirmed.",
        "The Aegis keeps choosing you.",
    ),
    b("Shuffled with everyone.", "Won anyway. Rude."),
    b("Winning is your whole bit.", "And the bit is working."),
];

const WIN_RATE_POPOFF_STRONG: &[Beat] = &[
    b("Welcome to winners queue.", "Population: mostly you."),
    b("You hit different.", "Like a Daedalus crit."),
    b("A healthy win rate.", "Unhealthy for the enemy."),
    b("The shuffler noticed.", "It's working on a counter."),
    b("Coin flip? Not for you.", "Weighted coin, apparently."),
    b("Wins: frequent.", "Humility: pending."),
    b("Your teammates noticed.", "Their MMR says thank you."),
    b("The enemy ancient trembled.", "Correctly."),
];

const WIN_RATE_POPOFF_NOTABLE: &[Beat] = &[
    b("Above the coin flip.", "Slightly weighted coin."),
    b("More wins than losses.", "Low bar. Cleared it."),
    b("A winning record.", "Smugness: earned."),
    b("Positive win rate.", "Your ancient mostly survived."),
];

const WIN_RATE_ROAST_EXTREME: &[Beat] = &[
    b("Lowest win rates: #{rank}", "The enemy's best teammate."),
    b("Service to the enemy team:", "Deeply appreciated."),
    b("The enemy carry wrote in.", "Thanks for the farm."),
    b("The other team's MVP.", "Wrong jersey, though."),
    b("Losing is a strategy.", "Yours, apparently."),
    b("Techies players called.", "They feel better now."),
    b("Free MMR dispenser.", "Restocked every game."),
    b("The shuffler balanced it.", "You rebalanced it. Downward."),
];

const WIN_RATE_ROAST_STRONG: &[Beat] = &[
    b("Coin flip merchant.", "Weighted coin. Wrong way."),
    b("Your W key looks broken.", "Have you tried pressing it?"),
    b("The enemy team says thanks.", "Same time next week?"),
    b("A generous win rate.", "Generous to the other team."),
    b("Every game a learning game.", "So much learning."),
    b("Wins were a rumor.", "Losses were the vibe."),
    b("Your throne had a timer.", "Tick, tick, boom."),
];

const WIN_RATE_ROAST_NOTABLE: &[Beat] = &[
    b("Just under the coin flip.", "So close to mediocre."),
    b("A few more L's than W's.", "Rounding error. Allegedly."),
    b("Slightly losing record.", "The shuffler says it's fine."),
    b("Almost a winning year.", "Almost."),
];

// Combat: deaths. The server's worst become a skill issue card.

const DEATHS_ROAST_EXTREME: &[Beat] = &[
    b("Most deaths: #{rank} of {of}", "Skill issue."),
    b("The fountain knows you.", "Skill issue."),
    b("Death had a busy year.", "Massive skill issue."),
    b("Grey screen, second monitor.", "Skill issue."),
    b("Space created. Allegedly.", "Skill issue, clearly."),
    b("Free gold for the enemy.", "Skill issue."),
    b("Not even Aegis could help.", "Massive skill issue."),
    b("Respawn timer, memorized.", "Skill issue, clearly."),
];

const DEATHS_ROAST_STRONG: &[Beat] = &[
    b("Respawn timer regular.", "The fountain knows your name."),
    b("You died. A lot.", "Feeding is just map control."),
    b("Grey screen time: plenty.", "Second monitor, basically."),
    b("Space created: lots.", "Mostly in the respawn timer."),
    b("A walking bounty rune.", "Grabbed on sight."),
    b("The enemy's net worth graph", "Sends its regards."),
    b("Buyback? On what gold?", "You gave it all away."),
    b("You tested every tower.", "Every tower won."),
];

const DEATHS_ROAST_NOTABLE: &[Beat] = &[
    b("A bit more dying than most.", "Space creator, part-time."),
    b("Death count: noticeable.", "Not alarming. Noticeable."),
    b("Respawn timer: familiar.", "Not best friends. Yet."),
    b("You died a fair bit.", "The fountain says hi."),
];

const DEATHS_POPOFF_EXTREME: &[Beat] = &[
    b("Fewest deaths: #{rank} of {of}", "Death is beneath you."),
    b("The fountain barely saw you.", "Healing is for the weak."),
    b("Aegis? Never needed one.", "You were the Aegis."),
    b("Death timer: rarely seen.", "Necrophos is still looking."),
    b("You simply did not die.", "Wraith King is jealous."),
    b("Enemies tried. Repeatedly.", "Not even close."),
    b("Unkillable, statistically.", "Your bounty was a myth."),
];

const DEATHS_POPOFF_STRONG: &[Beat] = &[
    b("You rarely died.", "Buyback gold, untouched."),
    b("Low deaths, high standards.", "A KDA you could eat off."),
    b("Hard to kill.", "Like a Puck in Phase Shift."),
    b("Your death count:", "Basically a typo."),
    b("You stayed alive.", "Your supports? Less so."),
    b("You played it safe.", "Coward or genius? Yes."),
    b("Respawn timer? Rarely.", "The fountain misses you."),
];

const DEATHS_POPOFF_NOTABLE: &[Beat] = &[
    b("Hard to catch.", "Mostly hiding in trees."),
    b("Fewer deaths than most.", "Survival instincts: online."),
    b("You died less than most.", "Cautious. Some say cowardly."),
    b("Respawn timer: occasional.", "A careful little hero."),
];

// Combat: kills and single games.

const KILLS_EXTREME: &[Beat] = &[
    b("Most kills a game: #{rank}", "Violence was the answer."),
    b("The kill feed was your blog.", "Updated every fight."),
    b("Your kill count", "Has its own kill count."),
    b("Kill-steal allegations:", "All true. No regrets."),
    b("Beyond Godlike, regularly.", "The announcer needs a break."),
    b("Your supports sent a memo.", "Leave some for us."),
    b("You saw a hero. It died.", "That's the whole strategy."),
    b("Bloodseeker took notes.", "Thirst: unquenched."),
];

const KILLS_STRONG: &[Beat] = &[
    b("You found the kills.", "Or stole them. Same thing."),
    b("Violence, often.", "Diplomacy, never."),
    b("Last hits on heroes, mostly.", "Supports remember."),
    b("The announcer knew you.", "Double kill. Triple kill."),
    b("You took fights.", "The fights didn't take you."),
    b("Your support had to ask:", "Can I have one?"),
    b("Kill participation: yes.", "Assists are for others."),
];

const KILLS_NOTABLE: &[Beat] = &[
    b("Kills found you.", "Some of them, anyway."),
    b("More kills than most.", "A healthy bloodlust."),
    b("You found kills.", "Occasionally the right ones."),
    b("Above-average violence.", "Respectable menace."),
];

const BEST_GAME_EXTREME: &[Beat] = &[
    b("Server game rank: #{rank}", "Absolute cinema."),
    b("Your magnum opus.", "Absolute cinema."),
    b("Screenshot the scoreboard.", "Absolute cinema, honestly."),
    b("Starring: you.", "Absolute cinema."),
    b("The game you'll talk about.", "Pure cinema."),
    b("Rated E: everyone died.", "Absolute cinema."),
    b("Peak {hero}.", "Absolute cinema."),
    b("One game. Your masterpiece.", "Absolute cinema, honestly."),
];

const BEST_GAME_STRONG: &[Beat] = &[
    b("Your best game.", "Absolute cinema."),
    b("Remember this one?", "Absolute cinema."),
    b("Your highlight reel.", "Pure cinema."),
    b("Peak {hero}.", "Absolute cinema."),
    b("Directed by: you.", "Absolute cinema, honestly."),
    b("Once, you were unstoppable.", "Absolute cinema."),
    b("Your best K/D/A all year.", "Pure cinema."),
];

const BEST_GAME_NOTABLE: &[Beat] = &[
    b("Your best game.", "Absolute cinema."),
    b("Your finest hour.", "Absolute cinema."),
    b("A game worth replaying.", "Pure cinema."),
    b("Remember this one?", "Absolute cinema."),
    b("Starring: you.", "Absolute cinema, honestly."),
    b("Your highlight reel.", "Absolute cinema."),
];

const KILL_LOSS_EXTREME: &[Beat] = &[
    b("Most kills in a loss: #{rank}", "Task failed successfully."),
    b("You won the deathmatch.", "Task failed successfully."),
    b("KDA player supreme.", "Task failed successfully."),
    b("The scoreboard says MVP.", "Task failed successfully."),
    b("Kills don't take towers.", "Task failed successfully."),
    b("You did your part.", "Task failed successfully."),
    b("You did everything. Almost.", "Task failed successfully."),
];

const KILL_LOSS_STRONG: &[Beat] = &[
    b("So many kills. Anyway,", "Task failed successfully."),
    b("You won the deathmatch.", "Task failed successfully."),
    b("Great stats. Wrong result.", "Task failed successfully."),
    b("Kills don't take towers.", "Task failed successfully."),
    b("You did your part.", "Task failed successfully."),
    b("The kill feed was all you.", "Task failed successfully."),
    b("Kills: plenty. Win: none.", "Task failed successfully."),
];

const KILL_LOSS_NOTABLE: &[Beat] = &[
    b("Plenty of kills.", "Task failed successfully."),
    b("You did your part.", "Task failed successfully."),
    b("Kills: yes. Win: no.", "Task failed successfully."),
    b("Great stats, one problem.", "Task failed successfully."),
    b("The scoreboard liked you.", "Task failed successfully."),
    b("The KDA looked great.", "Task failed successfully."),
];

// Farm. The server's poorest farmers become a skill issue card.

const GPM_POPOFF_EXTREME: &[Beat] = &[
    b("GPM rank: #{rank} of {of}", "Alchemist wants royalties."),
    b("Neutral camps: emptied.", "Restraining order pending."),
    b("Farming simulator champion.", "Last hits: a love language."),
    b("Anti-Mage saw your farm.", "And cried a little."),
    b("Every coin knew your name.", "And came running."),
    b("Net worth: offensive.", "Your supports felt that."),
    b("You farmed everything.", "Even the support's camp."),
];

const GPM_POPOFF_STRONG: &[Beat] = &[
    b("The creeps feared you.", "Rightfully."),
    b("Heavy farmer.", "The jungle had no chance."),
    b("Your farm was rich.", "Your supports? Not so much."),
    b("Gold came easy.", "Ask the jungle. It remembers."),
    b("Your Midas: metaphorical.", "Also literal."),
    b("Last hits: a lifestyle.", "Heroes? Later. Maybe."),
    b("PvE specialist.", "The heroes were optional."),
];

const GPM_POPOFF_NOTABLE: &[Beat] = &[
    b("Respectable farm.", "Nothing to apologize for."),
    b("Above-average farm.", "The jungle noticed."),
    b("You farmed well.", "Not obscenely. Well."),
    b("Decent gold income.", "Creeps respect you."),
];

const GPM_ROAST_EXTREME: &[Beat] = &[
    b("Lowest GPM: #{rank} of {of}", "Skill issue."),
    b("Last hits: optional.", "Skill issue."),
    b("The creeps survived you.", "Massive skill issue."),
    b("Your farm: theoretical.", "Skill issue."),
    b("Gold per minute, barely.", "Skill issue, clearly."),
    b("Midas would be insulted.", "Skill issue."),
    b("The jungle felt safe.", "Massive skill issue."),
];

const GPM_ROAST_STRONG: &[Beat] = &[
    b("Your gold income:", "A rounding error."),
    b("The creeps survived you.", "They tell their friends."),
    b("Space creator, supposedly.", "The space stayed empty."),
    b("You never had items.", "Just vibes and a TP."),
    b("Your farm, in Dota terms:", "Pos 5 on a good day."),
    b("Midas? You needed one.", "Badly. Twice."),
    b("Gold went everywhere", "Except your hero."),
];

const GPM_ROAST_NOTABLE: &[Beat] = &[
    b("Gold income: modest.", "A support budget, basically."),
    b("Farm: below average.", "The jungle didn't miss you."),
    b("Light on the gold.", "Supports call it normal."),
    b("A modest farmer.", "The creeps barely noticed."),
];

const LAST_HITS_EXTREME: &[Beat] = &[
    b("Last-hit rank: #{rank} of {of}", "Creep waves: deleted."),
    b("CS like a metronome.", "Click. Gold. Click. Gold."),
    b("More waves than the ocean.", "Every one, last-hit."),
    b("The creeps feared the click.", "The click never missed."),
    b("Last-hitting is an art.", "You're the museum."),
    b("The ranged creep feared you.", "Melee ones too."),
    b("Creep equilibrium? Yours.", "Every wave, on schedule."),
];

const LAST_HITS_STRONG: &[Beat] = &[
    b("Last hits, lovingly.", "Every creep got the click."),
    b("The creeps clocked in.", "You clocked them out."),
    b("Lane. Jungle. Ancients.", "All farmed. No survivors."),
    b("You never missed a creep.", "Heroes? Different story."),
    b("Quelling Blade enjoyer.", "Creeps never stood a chance."),
    b("Creeps per minute: many.", "Feelings per minute: none."),
    b("Your lane was a factory.", "Creeps in, gold out."),
    b("CS so clean it squeaks.", "The lane was spotless."),
];

const LAST_HITS_NOTABLE: &[Beat] = &[
    b("Decent farm habits.", "The creeps respected it."),
    b("Solid last hits.", "Creeps feared you a little."),
    b("Good CS.", "Your lane partner noticed."),
    b("Above-average farming.", "Click, gold, repeat."),
];

const HERO_DAMAGE_EXTREME: &[Beat] = &[
    b("Damage rank: #{rank} of {of}", "Health bars feared you."),
    b("Enemy health bars: gone.", "You did that."),
    b("Damage dealt: excessive.", "Daedalus crits, all year."),
    b("Their support's mana bar:", "Empty, thanks to you."),
    b(
        "You didn't farm. You hunted.",
        "Creeps were an afterthought.",
    ),
    b("You started every fight.", "And finished most of them."),
    b("Glass cannon? Just cannon.", "The glass was theirs."),
];

const HERO_DAMAGE_STRONG: &[Beat] = &[
    b("Big damage numbers.", "The kind people screenshot."),
    b("You hit heroes. Often.", "Creeps can wait."),
    b("Damage dealt: plenty.", "Damage taken: don't ask."),
    b("The enemy felt you.", "Mostly in the health bar."),
    b("You were in every fight.", "Loudly."),
    b("Spell spam, deployed.", "Mana is a suggestion."),
    b("Spells landed. On heroes.", "Imagine that."),
    b("Right-clicks that hurt.", "Every hit landed somewhere."),
];

const HERO_DAMAGE_NOTABLE: &[Beat] = &[
    b("Damage: respectable.", "You hit things. Good."),
    b("Solid hero damage.", "The fights noticed you."),
    b("You dealt damage.", "Respectably. Not obscenely."),
    b("Above-average damage.", "Enemies took the hint."),
];

const HEALING_EXTREME: &[Beat] = &[
    b("Healing rank: #{rank} of {of}", "A fountain with legs."),
    b("The fountain felt replaced.", "You were the fountain now."),
    b("Reliable like a Treant heal.", "Thanks were not given."),
    b("You healed everyone.", "Nobody said thank you."),
    b("Health bars went up.", "Gratitude did not."),
    b("Certified healbot.", "Your carry owes you a Midas."),
    b("Mana spent on others:", "All of it. Every game."),
];

const HEALING_STRONG: &[Beat] = &[
    b("You kept people alive.", "Some of them deserved it."),
    b("Heals, delivered.", "Tips not included."),
    b("Your team's health bar:", "Your problem, apparently."),
    b("Healer main energy.", "The carry took the credit."),
    b("You topped everyone up.", "Nobody topped you up."),
    b("A walking Healing Salve.", "Infinite charges."),
    b("You healed the carry.", "The carry farmed instead."),
    b("Healing numbers: huge.", "Fame: still the carry's."),
];

const HEALING_NOTABLE: &[Beat] = &[
    b("A few heals here.", "A few saves there."),
    b("Healing: above average.", "Your team noticed. Barely."),
    b("You patched people up.", "Bandages, mostly."),
    b(
        "A decent amount of healing.",
        "Nobody said thanks. Classic.",
    ),
];

const STUNS_EXTREME: &[Beat] = &[
    b("Stun rank: #{rank} of {of}", "Nobody moved. Ever."),
    b("Enemies spent the year", "Stunned. By you."),
    b("You said hold still.", "They held still."),
    b("Stun lock specialist.", "BKB was their only hope."),
    b("They bought BKB for you.", "Specifically you."),
    b("Crowd control: total.", "The crowd was controlled."),
    b("Enemy movement speed:", "Zero, mostly."),
];

const STUNS_STRONG: &[Beat] = &[
    b("Lots of stuns.", "Lots of stars over heads."),
    b("You chain-stunned.", "They're still dizzy."),
    b("Enemies stood still a lot.", "Your doing."),
    b("Stuns, delivered:", "Faster than the courier."),
    b("Disable duty: done.", "Repeatedly. Gleefully."),
    b("BKB timings were about you.", "Correctly."),
    b("Stuns on cooldown.", "Never for long."),
    b("You froze the fight.", "Literally. Repeatedly."),
];

const STUNS_NOTABLE: &[Beat] = &[
    b("A few good stuns.", "The right ones, sometimes."),
    b("Decent stun time.", "Enemies noticed eventually."),
    b("Some solid stuns.", "Hold still, please."),
    b("A respectable disabler.", "Stars over a few heads."),
];

// Map.

const WARDS_POPOFF_EXTREME: &[Beat] = &[
    b("Ward rank: #{rank} of {of}", "The fog of war fears you."),
    b("Vision wins games.", "You bought all of it."),
    b("Your minimap glowed.", "Nobody looked at it."),
    b("Night Stalker hated you.", "Night was cancelled."),
    b("Pos 5 suffering, mastered.", "The wards thank you."),
    b("The wards never stopped.", "Neither did the dewards."),
];

const WARDS_POPOFF_STRONG: &[Beat] = &[
    b("You warded. A lot.", "Your carry ignored all of it."),
    b("Observer ward enjoyer.", "Free wards, placed with love."),
    b("Vision provided.", "Thanks not provided."),
    b("Map awareness: yours.", "Everyone else: still blind."),
];

const WARDS_POPOFF_NOTABLE: &[Beat] = &[
    b("Some wards placed.", "The map was grateful."),
    b("Decent vision game.", "Not legendary. Decent."),
    b("You placed wards.", "More than most can say."),
];

const WARDS_ROAST_EXTREME: &[Beat] = &[
    b("Fewest wards: #{rank} of {of}", "Vision is for the weak."),
    b("Vision game: absent.", "Smoke ganks loved you."),
    b("Observer wards are free.", "Still too expensive."),
    b("The fog was your home.", "Rent-free."),
];

const WARDS_ROAST_STRONG: &[Beat] = &[
    b("The map stayed dark.", "Mood lighting, apparently."),
    b("Your ward budget:", "Zero. Even when they're free."),
    b("You played in the fog.", "Bold. Very bold."),
    b("Sentries? Never met them.", "Invisible heroes thank you."),
];

const WARDS_ROAST_NOTABLE: &[Beat] = &[
    b("Light on the wards.", "The fog thanks you."),
    b("A few wards short.", "Your support noticed."),
    b("Vision: optional.", "You opted out. Mostly."),
];

const STACKS_EXTREME: &[Beat] = &[
    b("Stack rank: #{rank} of {of}", "The carry ate every stack."),
    b("Camps stacked: so many.", "The ancients filed a report."),
    b("You pulled at x:53.", "Every minute, on time."),
    b("Triple stacks, routinely.", "Your carry ate well."),
];

const STACKS_STRONG: &[Beat] = &[
    b("Camps stacked:", "Higher than Medusa at 50."),
    b("Every x:53, on the clock.", "The jungle was sorted."),
    b("You stacked camps.", "Nobody farmed them. Classic."),
];

const STACKS_NOTABLE: &[Beat] = &[
    b("Some stacks.", "The carry may have noticed."),
    b("A few camps stacked.", "Good habits, sort of."),
    b("You stacked occasionally.", "When you remembered x:53."),
];

const RUNES_EXTREME: &[Beat] = &[
    b("Rune rank: #{rank} of {of}", "Every rune, yours."),
    b("Runes taken: all of them.", "Mid was not consulted."),
    b("Double damage? Yours.", "Haste? Yours. Bounty? Yours."),
    b("You lived at the river.", "Rune spawns set your alarm."),
];

const RUNES_STRONG: &[Beat] = &[
    b("The river was yours.", "Runes rarely escaped."),
    b("A rune goblin.", "Bounties especially."),
    b("Every two minutes:", "Rune time."),
];

const RUNES_NOTABLE: &[Beat] = &[
    b("Some runes grabbed.", "Bounty, mostly."),
    b("You found runes.", "Occasionally the good ones."),
    b("Rune control: present.", "Not dominant. Present."),
];

const COURIERS_EXTREME: &[Beat] = &[
    b("Courier rank: #{rank} of {of}", "No delivery was safe."),
    b("Couriers sniped:", "Their Tangoes never arrived."),
    b("You hunted couriers.", "The donkey union is upset."),
    b("Bottle refills denied.", "Mid was furious."),
];

const COURIERS_STRONG: &[Beat] = &[
    b("Couriers fell to you.", "Packages: undelivered."),
    b("Courier sniper.", "Free gold, gift-wrapped."),
    b("Their BKB was on the way.", "It was."),
];

const COURIERS_NOTABLE: &[Beat] = &[
    b("A courier or two.", "Opportunistic."),
    b("You sniped some couriers.", "The donkeys remember."),
    b("Deliveries interrupted.", "Occasionally, on purpose."),
];

// Chaos.

const PINGS_EXTREME: &[Beat] = &[
    b("Ping rank: #{rank} of {of}", "Teammates heard every one."),
    b("You pinged. A lot.", "The map needs earplugs."),
    b("Alt-click enthusiast.", "Everyone knows your mana."),
    b("Your minimap was a rave.", "Ping, ping, ping, ping."),
    b("You communicated.", "Exclusively in pings."),
    b("The ? ping got a workout.", "So did your teammates."),
    b("Pings per game: concerning.", "The minimap needs a nap."),
];

const PINGS_STRONG: &[Beat] = &[
    b("Pings sent: plenty.", "Read: a few."),
    b("Communication: loud.", "Mostly exclamation marks."),
    b("Danger ping, every fight.", "After the fight. Usually."),
    b("You pinged for help.", "Help was busy farming."),
    b("Teammates heard you.", "Every single time."),
    b("You pinged the Roshan pit.", "Repeatedly. Nobody came."),
    b("The map got pinged.", "All of it. Twice."),
    b("Silence was an option.", "You declined it."),
];

const PINGS_NOTABLE: &[Beat] = &[
    b("Pings: a few extra.", "The minimap survived."),
    b("A chatty pinger.", "Your team got the memo."),
    b("Above-average pings.", "Somebody had to."),
    b("You pinged more than most.", "The minimap noticed."),
];

const APM_EXTREME: &[Beat] = &[
    b("APM rank: #{rank} of {of}", "Fast hands. Faster clicks."),
    b("Your mouse wants a break.", "Your keyboard too."),
    b("Clicks per minute: absurd.", "Invoker would be proud."),
    b("You clicked a lot.", "Some of them mattered."),
    b("Micro: elite.", "Meepo players nodded."),
    b("Hands moving constantly.", "Hero moving occasionally."),
    b("Your keyboard wants a word.", "Several, actually."),
    b("Click speed: concerning.", "The mouse wants hazard pay."),
];

const APM_STRONG: &[Beat] = &[
    b("Busy hands.", "Some actions were useful."),
    b("Your right-click rate:", "Concerning."),
    b("Lots of clicks.", "Spamming move counts, right?"),
    b("You stayed busy.", "Mostly clicking the ground."),
    b("Invoker fingers.", "Even on Ogre."),
    b("Actions per minute: many.", "Decisions per minute: fewer."),
    b("Stop-and-go, all game.", "The S key needs a rest."),
    b("Shift-queue master.", "Every waypoint, planned."),
];

const APM_NOTABLE: &[Beat] = &[
    b("Above-average APM.", "Your mouse is tired."),
    b("Quick fingers.", "The keyboard felt it."),
    b("Busy-ish hands.", "Respectable clicking."),
    b("You click with purpose.", "Usually."),
];

// Throws are team gold leads, so the setups say "your team".

const THROW_EXTREME: &[Beat] = &[
    b("Biggest throw: #{rank} of {of}", "This is fine."),
    b("Your team was up", "This is fine."),
    b("Your team's peak lead:", "This is fine. It's fine."),
    b("Your team had this one.", "This is fine."),
    b("Your team saw the throne.", "This is fine. Totally."),
    b("Your team was cruising.", "This is fine."),
    b("A comfortable team lead.", "This is fine. It's fine."),
];

const THROW_STRONG: &[Beat] = &[
    b("Your team was up", "This is fine."),
    b("Peak team gold lead:", "This is fine."),
    b("Your team was so far ahead.", "This is fine. Totally."),
    b("Your team had this one.", "This is fine. It's fine."),
    b("Your team, at its peak:", "This is fine."),
    b("Your team was cruising.", "This is fine."),
];

const THROW_NOTABLE: &[Beat] = &[
    b("Your team was up", "This is fine."),
    b("Your team led by", "This is fine. It's fine."),
    b("A solid team lead.", "This is fine."),
    b("Your team was ahead.", "This is fine. Totally."),
    b("Peak team gold lead:", "This is fine."),
    b("Your team had it.", "This is fine."),
];

const COMEBACK_EXTREME: &[Beat] = &[
    b(
        "Comeback rank: #{rank} of {of}",
        "Their throw, your highlight.",
    ),
    b("Your team was buried.", "And then it wasn't."),
    b("Your team was losing.", "Key word: was."),
    b("Lost cause, they said.", "They said wrong."),
    b("Down bad, as a team.", "Up good, eventually."),
    b("Your team was down", "The enemy got comfortable."),
    b("The enemy had this one.", "They did not have this one."),
];

const COMEBACK_STRONG: &[Beat] = &[
    b("Your team was down", "Then the enemy got greedy."),
    b("Gold deficit? Erased.", "Like it never happened."),
    b("Behind on gold.", "Ahead on stubbornness."),
    b("The enemy relaxed.", "Big mistake."),
    b("Your team refused to lose.", "Rude, honestly."),
    b("Never trust a lead.", "Especially theirs."),
    b("Your team trailed.", "Briefly, it turned out."),
    b("The enemy counted chickens.", "Before the throne fell."),
];

const COMEBACK_NOTABLE: &[Beat] = &[
    b("Your team clawed back.", "Slowly, then all at once."),
    b("Your team came back.", "From a respectable deficit."),
    b("Behind, then not.", "Classic."),
    b("A tidy comeback.", "The enemy relaxed. Oops."),
];

const RAPIERS_POPOFF_EXTREME: &[Beat] = &[
    b("Rapier rank: #{rank} of {of}", "Fortune favors the bold."),
    b("You bought Divine Rapier.", "And won. Respect."),
    b("Big sword energy.", "It worked. Somehow."),
    b("All-in on a sword.", "The sword delivered."),
    b("Rapier bought, game won.", "Most people can't say that."),
    b("Hubris, but it paid off.", "This time."),
    b("Rapier, then the throne.", "In that order."),
    b("You bet the farm on a sword.", "The farm paid off."),
];

const RAPIERS_POPOFF_STRONG: &[Beat] = &[
    b("You bought a Rapier.", "Your team prayed. It worked."),
    b("Sword acquired.", "Ancient destroyed."),
    b("All your gold on a sword.", "Worth every coin."),
    b("The sword gamble.", "Paid out."),
    b("The bravest purchase.", "Rewarded, for once."),
    b("Divine Rapier: bought.", "Dropped: never. Allegedly."),
    b("Sword in hand.", "Ancient in pieces."),
];

const RAPIERS_POPOFF_NOTABLE: &[Beat] = &[
    b("Rapier purchased.", "Results: positive."),
    b("You bought a Rapier.", "And lived to tell it."),
    b("A Rapier appeared.", "The throne fell. Theirs."),
    b("Sword gamble:", "Successful."),
];

const RAPIERS_ROAST_EXTREME: &[Beat] = &[
    b("Rapier rank: #{rank} of {of}", "And still lost with one."),
    b("Divine Rapier purchased.", "Divine intervention: denied."),
    b("Big sword, big hubris.", "Big loss."),
    b("All-in on a sword.", "The sword folded."),
    b("Hubris has a shape.", "It's a sword."),
    b("You went Rapier.", "Your ancient went anyway."),
    b("The sword was divine.", "The outcome was not."),
    b("Rapier: bought with hope.", "Hope was not enough."),
];

const RAPIERS_ROAST_STRONG: &[Beat] = &[
    b("You bought a Rapier.", "Your team still lost. Wild."),
    b("Sword acquired.", "Game not acquired."),
    b("Bold item choice.", "Bold outcome too."),
    b("Rapier: purchased.", "Throne: not defended."),
    b("Big sword energy.", "Small win energy."),
    b("Divine Rapier.", "Divine punishment."),
    b("Rapier, then regret.", "In that order."),
];

const RAPIERS_ROAST_NOTABLE: &[Beat] = &[
    b("One sword, one loss.", "Balanced, technically."),
    b("A Rapier happened.", "So did a loss."),
    b("You tried the sword.", "The sword tried back."),
    b("Rapier, then a loss.", "Classic Dota."),
];

// Clutch: odds, streaks, and rating.

const UPSET_WIN_EXTREME: &[Beat] = &[
    b("Biggest upset: #{rank} of {of}", "Calculated."),
    b("The math said lose.", "Calculated."),
    b("Ratings said you'd lose.", "Calculated. Obviously."),
    b("Nobody saw this coming.", "Totally calculated."),
    b("Statistically, a loss.", "Calculated."),
    b("Underdogs, on paper.", "Calculated. Obviously."),
    b("The odds wrote you off.", "Totally calculated."),
];

const UPSET_WIN_STRONG: &[Beat] = &[
    b("You won at", "Calculated."),
    b("The math was against you.", "Calculated."),
    b("Expected result: a loss.", "Calculated. Obviously."),
    b("Your odds going in:", "Calculated."),
    b("Underdogs, on paper.", "Totally calculated."),
    b("The ratings said no.", "Calculated."),
    b("Paper says you lose.", "Totally calculated."),
];

const UPSET_WIN_NOTABLE: &[Beat] = &[
    b("You won at", "Calculated."),
    b("Not favored. Won anyway.", "Calculated."),
    b("Underdogs that day.", "Calculated. Obviously."),
    b("The odds said no.", "Calculated."),
    b("A sneaky upset.", "Totally calculated."),
    b("Against the odds.", "Calculated."),
];

const UPSET_LOSS_EXTREME: &[Beat] = &[
    b("Biggest upset loss: #{rank}", "The math cried."),
    b("Favored to win.", "Favored. Past tense."),
    b("The odds loved you.", "You did not love them back."),
    b("A sure thing.", "Surely not."),
    b("Free win, on paper.", "Paper lost."),
    b("Expected result: a win.", "Actual result: a lesson."),
    b("The math was on your side.", "You weren't."),
];

const UPSET_LOSS_STRONG: &[Beat] = &[
    b("You were favored.", "Briefly."),
    b("Easy game, they said.", "They lied. You helped."),
    b("The odds said win.", "Your team said no."),
    b("On paper: a stomp.", "In practice: a stomping."),
    b("Favorites, going in.", "Underdogs, coming out."),
    b("Free MMR, allegedly.", "For the other team."),
    b("Win chance: high.", "Win: no."),
    b("Favored. Confident.", "Incorrect."),
];

const UPSET_LOSS_NOTABLE: &[Beat] = &[
    b("Slight favorites.", "Slight problem."),
    b("Favored, then not.", "Happens. Mostly to you."),
    b("The odds were good.", "The game was not."),
    b("Should have won.", "Didn't. Classic."),
];

const WIN_STREAK_EXTREME: &[Beat] = &[
    b("Streak rank: #{rank} of {of}", "Rampage, but for games."),
    b("You couldn't lose.", "You tried. Probably."),
    b("Win, win, win, win.", "And then some more."),
    b("The heater was on.", "Call the fire department."),
    b("The shuffler gave up.", "You kept winning anyway."),
    b("Winning became routine.", "Boring, honestly. For them."),
    b("Losing? Briefly forgotten.", "It remembered eventually."),
];

const WIN_STREAK_STRONG: &[Beat] = &[
    b("A hot streak.", "Enemies felt the heat."),
    b("Wins in a row.", "Your queue button glowed."),
    b("You got on a roll.", "Multicast, but for wins."),
    b("The streak was real.", "So was the smugness."),
    b("Back-to-back-to-back.", "And then some."),
    b("Losing was off the menu.", "For a while."),
    b("Win after win.", "The lobby sighed."),
    b("A streak, unbroken.", "For a glorious while."),
];

const WIN_STREAK_NOTABLE: &[Beat] = &[
    b("A small heater.", "Warm, not hot."),
    b("A nice little streak.", "Briefly unstoppable."),
    b("A few wins running.", "The good times rolled."),
    b("You strung some wins.", "Then reality returned."),
];

const LOSS_STREAK_EXTREME: &[Beat] = &[
    b("Loss streak rank: #{rank}", "gg go next"),
    b("Queue, lose, repeat.", "gg go next"),
    b("Tilt, visualized:", "gg wp go next"),
    b("One more game, they said.", "gg go next"),
    b("Requeue. Requeue. Requeue.", "gg go next"),
    b("The losing stretch:", "gg wp go next"),
    b("Tilt queue, activated.", "gg go next"),
];

const LOSS_STREAK_STRONG: &[Beat] = &[
    b("A rough stretch.", "gg go next"),
    b("Queue, lose, repeat.", "gg go next"),
    b("Tilt, visualized:", "gg wp go next"),
    b("Requeue, immediately.", "gg go next"),
    b("The losing streak:", "gg go next"),
    b("Rough patch, measured:", "gg wp go next"),
];

const LOSS_STREAK_NOTABLE: &[Beat] = &[
    b("A losing run.", "gg go next"),
    b("Not your stretch.", "gg go next"),
    b("Queue, lose, repeat.", "gg wp go next"),
    b("A small slump.", "gg go next"),
    b("Tilt, mildly:", "gg go next"),
    b("Losses, stacked:", "gg wp go next"),
];

const RATING_POPOFF_EXTREME: &[Beat] = &[
    b("Rating gain rank: #{rank}", "Elevator, not stairs."),
    b("Biggest single-game gain:", "Rating go brrrr."),
    b("One game, big jump.", "The ladder had a shortcut."),
    b("You won the big one.", "The rating paid out."),
    b("One win, huge payout.", "Better than gamba."),
    b("Rating, launched.", "Houston, we have MMR."),
    b("One game, all the points.", "Greedy, honestly."),
    b("The rating jumped.", "So did you, probably."),
];

const RATING_POPOFF_STRONG: &[Beat] = &[
    b("A big rating jump.", "Broodmother up a cliff."),
    b("One game, real gains.", "The grind paid off."),
    b("The rating gods smiled.", "Once. Generously."),
    b("Big win, big number.", "Frame it."),
    b("Elo, inflated.", "Personally, by you."),
    b("Up in one go.", "Elevator, right direction."),
    b("A rating spike.", "Enjoy the view."),
];

const RATING_POPOFF_NOTABLE: &[Beat] = &[
    b("Rating, nudged up.", "Progress, technically."),
    b("A nice rating bump.", "One good game."),
    b("Rating went up.", "Noticeably, even."),
    b("A solid single-game gain.", "The ladder approved."),
];

const RATING_ROAST_EXTREME: &[Beat] = &[
    b("Rating loss rank: #{rank}", "Terminal velocity."),
    b("Biggest single-game drop:", "Pudge hook, straight down."),
    b("One game did that.", "To your rating. Ouch."),
    b("You lost the big one.", "The ladder had a trapdoor."),
    b("One loss, big price.", "Expensive, like a buyback."),
    b("The descent was artistic.", "Graceful, really."),
    b("One loss. Huge bill.", "No refunds."),
    b("The rating fell.", "So did the throne."),
];

const RATING_ROAST_STRONG: &[Beat] = &[
    b("A big rating drop.", "Recalibration waiting room."),
    b("One game hurt.", "Your rating still feels it."),
    b("Down in one go.", "Elevator, wrong direction."),
    b("The rating took a hit.", "Buyback couldn't save it."),
    b("You lost one big one.", "The math charged extra."),
    b("A rough single game.", "The ladder remembers."),
    b("A costly loss.", "The ladder kept the change."),
];

const RATING_ROAST_NOTABLE: &[Beat] = &[
    b("A rating wobble.", "Nothing a win can't fix."),
    b("A rating dip.", "Recoverable. Probably."),
    b("One game cost you.", "Not a lot. Enough."),
    b("Rating went down.", "Briefly. Hopefully."),
];

// Time. Clock readings are when a game finished, never when it started.

const NIGHT_OWL_EXTREME: &[Beat] = &[
    b("Your latest finish:", "One more game."),
    b("Still playing at", "Just one more game."),
    b("Last game ended at", "One more game."),
    b("The final ancient fell at", "One more game. Probably."),
    b("Final lobby ended at", "One more game."),
    b("Game over, finally, at", "Just one more game."),
    b("Late-night games rank: #{rank}", "One more game."),
];

const NIGHT_OWL_STRONG: &[Beat] = &[
    b("Your latest finish:", "One more game."),
    b("Still playing at", "One more game. Probably."),
    b("Last game ended at", "Just one more game."),
    b("Your Dota day ended at", "One more game."),
    b("The final ancient fell at", "One more game."),
    b("The last lobby closed at", "Just one more game."),
];

const NIGHT_OWL_NOTABLE: &[Beat] = &[
    b("Your latest finish:", "One more game."),
    b("Still playing at", "Just one more game."),
    b("Last game ended at", "One more game."),
    b("Your Dota day ended at", "One more game. Probably."),
    b("Wrapped up at", "One more game."),
    b("The last ancient fell at", "Just one more game."),
];

const MARATHON_EXTREME: &[Beat] = &[
    b(
        "Most games in a day: #{rank}",
        "The queue button needs rest.",
    ),
    b("One day, all Dota.", "Respect, and concern."),
    b("The longest day.", "One more game, times ten."),
    b("You played all day.", "The lobby had a resident."),
    b("Marathon mode: on.", "Hydrate, please."),
];

const MARATHON_STRONG: &[Beat] = &[
    b("A long day of Dota.", "No regrets. Few breaks."),
    b("Game after game.", "After game. After game."),
    b("You binged.", "Dota, the series."),
    b("Dota, all day.", "Respect the commitment."),
];

const MARATHON_NOTABLE: &[Beat] = &[
    b("A busy day of Dota.", "Just a few more."),
    b("A long session.", "Respectable stamina."),
    b("Several games, one day.", "Warmed up, at least."),
];

// Heroes.

const LOYAL_POPOFF_EXTREME: &[Beat] = &[
    b("{hero}, again.", "Why learn anything else?"),
    b("Loyalty rank: #{rank} of {of}", "One hero. Many wins."),
    b("Comfort pick, every pick.", "Comfort castle, really."),
    b("Ban this hero.", "Watch what happens."),
    b("First pick. Only pick.", "And it worked."),
    b("One hero. All year.", "If it's not broken..."),
    b("You and {hero}:", "Basically one account."),
];

const LOYAL_POPOFF_STRONG: &[Beat] = &[
    b("A main, and it shows.", "The wins show too."),
    b("Loyal to one hero.", "The hero was loyal back."),
    b("Same hero, same result.", "A win, mostly."),
    b("Why switch?", "It works."),
    b("Comfort zone: located.", "Moved in. Decorated."),
    b("{hero} main.", "And it shows in the wins."),
    b("The same hero, again.", "The enemy groaned."),
    b("Hero picked. Game won.", "Lather, rinse, repeat."),
];

const LOYAL_POPOFF_NOTABLE: &[Beat] = &[
    b("A trusty pick.", "It trusted you back."),
    b("A favorite hero.", "Favorite for a reason."),
    b("You had a go-to.", "It went well."),
    b("Comfort pick, deployed.", "Comfortably."),
];

const LOYAL_ROAST_EXTREME: &[Beat] = &[
    b("{hero}, again?", "The hero has asked for space."),
    b("Loyal to a fault.", "The fault is the win rate."),
    b("One hero. Many losses.", "Love is blind."),
    b("You kept picking it.", "It kept losing."),
    b("Comfort pick, allegedly.", "Comfortable for the enemy."),
    b("Ban this hero.", "For your own good."),
    b("Loyalty rank: #{rank} of {of}", "Results rank: don't ask."),
];

const LOYAL_ROAST_STRONG: &[Beat] = &[
    b("A main with a losing record.", "Commitment: unshakable."),
    b("First pick, every pick.", "The enemy said thanks."),
    b("Same hero, same result.", "The result was a loss."),
    b("You believe in this hero.", "It doesn't believe back."),
    b("Loyalty: admirable.", "Win rate: less so."),
    b("It's not the hero.", "It might be the hero."),
    b("Picked it again.", "Lost with it again."),
    b("Same hero, all year.", "Same lesson, all year."),
];

const LOYAL_ROAST_NOTABLE: &[Beat] = &[
    b("A favorite hero.", "Unrequited, mostly."),
    b("A favorite, not a winner.", "Love is complicated."),
    b("Your go-to hero.", "It went poorly."),
    b("Loyal to a losing hero.", "Respect, honestly."),
];

const HERO_POOL_POPOFF_EXTREME: &[Beat] = &[
    b(
        "Hero pool rank: #{rank} of {of}",
        "Your hero pool is an ocean.",
    ),
    b("Random? No. All of them.", "Every hero is your hero."),
    b("Know all their spells?", "Even the Invoker ones?"),
    b("Flex pick incarnate.", "Draft phase can't scare you."),
    b("You played everything.", "Rubick is taking notes."),
    b("A hero grid rainbow.", "Mastery is a different stat."),
    b("All-pick, literally.", "You picked all of them."),
    b("Hero grid: explored.", "Every corner, every role."),
];

const HERO_POOL_POPOFF_STRONG: &[Beat] = &[
    b("Jack of all trades.", "Master of... some?"),
    b("Variety, but make it Dota.", "Your teammates never knew."),
    b("New hero, who dis?", "Every other game."),
    b("Flex pick energy.", "Whatever's left, you play."),
    b("So many heroes.", "So many wiki tabs open."),
    b("You tried them all.", "Some of them twice."),
    b("Random button? No need.", "You are the random button."),
    b("A new hero, often.", "The guide tab stayed open."),
];

const HERO_POOL_POPOFF_NOTABLE: &[Beat] = &[
    b("A varied year.", "Never boring."),
    b("A healthy hero pool.", "Flexible, not chaotic."),
    b("You mixed it up.", "Variety, with taste."),
    b("More heroes than most.", "A curious drafter."),
];

const HERO_POOL_ROAST_EXTREME: &[Beat] = &[
    b("Narrowest pool: #{rank} of {of}", "Hero puddle, certified."),
    b("Hero pool: puddle.", "A very shallow puddle."),
    b("Ban two heroes.", "Watch you crumble."),
    b("Other heroes exist.", "Allegedly."),
    b("Hero select screen:", "Mostly ignored."),
    b("Same few heroes.", "Every. Single. Game."),
    b("The hero grid is large.", "You found a corner."),
    b("Draft phase, solved.", "Same answer every time."),
];

const HERO_POOL_ROAST_STRONG: &[Beat] = &[
    b("A short hero list.", "Very short."),
    b("Your hero pool:", "Kiddie pool, really."),
    b("Comfort zone? Fortress.", "Moat included."),
    b("Draft phase was short.", "You knew the answer."),
    b("Why learn new heroes?", "Valid question, honestly."),
    b("You found your heroes.", "And never left them."),
    b("New heroes? Not today.", "Or any day, really."),
    b("Patch notes came out.", "Your picks didn't change."),
];

const HERO_POOL_ROAST_NOTABLE: &[Beat] = &[
    b("Familiar picks.", "Comfort food, really."),
    b("A tight hero pool.", "Focused. Or stuck."),
    b("A few favorites.", "Mostly the same few."),
    b("Hero variety: modest.", "Comfort over chaos."),
];

// Items. Counts come from parsed games, so lines speak to habits.

const BUYBACKS_EXTREME: &[Beat] = &[
    b("Buyback rank: #{rank} of {of}", "Death was a speed bump."),
    b("Bought back. Often.", "Died again. Often."),
    b("You bought your way back.", "Into another death."),
    b("Buyback, every fight.", "Gold never stood a chance."),
];

const BUYBACKS_STRONG: &[Beat] = &[
    b("Buyback enjoyer.", "Gold is temporary."),
    b("Dead? Buy back.", "Dead again? Buy back."),
    b("Lots of buybacks.", "Lots of reasons to."),
    b("Fountain, then back out.", "Then back to the fountain."),
];

const BUYBACKS_NOTABLE: &[Beat] = &[
    b("A few buybacks.", "Some were even needed."),
    b("You bought back.", "Mostly worth it."),
    b("Buyback gold: used.", "Occasionally wisely."),
];

const ITEM_ANY: &[Beat] = &[
    b("Same item, every chance.", "Build guides are optional."),
    b("Your favorite purchase.", "The shop knows your order."),
    b("Shopping habit: located.", "Same item. Again."),
    b("The shopkeeper knows you.", "By your usual."),
];

/// Tracked item key, its display name, and its lines.
pub(super) const ITEM_POOLS: &[(&str, &str, &[Beat])] = &[
    (
        "magic_wand",
        "Magic Wand",
        &[
            b("Magic Wand appreciation.", "Always one in the cart."),
            b("Wand first. Questions later.", "Iron Branches were harmed."),
            b("The wand is the build.", "The rest is decoration."),
            b("Magic Wand, again.", "Spell spam? Bring it."),
        ],
    ),
    (
        "black_king_bar",
        "Black King Bar",
        &[
            b("BKB, again and again.", "Magic immunity, emotionally."),
            b("Spell immune, mostly.", "Also immune to advice."),
            b("Black King Bar enjoyer.", "Spells are a rumor."),
            b("BKB in the shop cart.", "Permanently."),
        ],
    ),
    (
        "blink",
        "Blink Dagger",
        &[
            b("Blink Dagger, again.", "Into five heroes, usually."),
            b("You blinked a lot.", "Into fights. Out of luck."),
            b("Blink first, think later.", "Classic initiator."),
        ],
    ),
    (
        "hand_of_midas",
        "Hand of Midas",
        &[
            b("Midas, habitually.", "Greed is a build."),
            b("Hand of Midas enjoyer.", "Gold now, fights never."),
            b("You bought Midas.", "Your team bought time."),
        ],
    ),
    (
        "smoke_of_deceit",
        "Smoke of Deceit",
        &[
            b("Smoke of Deceit, often.", "Ganks? Occasionally."),
            b("Smoke, bought in bulk.", "The plan was always a gank."),
            b("Smoke purchased.", "Ganks planned. Allegedly."),
        ],
    ),
    (
        "dust",
        "Dust of Appearance",
        &[
            b("Dust of Appearance, often.", "Riki has trust issues."),
            b("Dust, always in stock.", "Paranoia, well placed."),
            b("You brought the Dust.", "Invisible heroes, exposed."),
        ],
    ),
    (
        "bottle",
        "Bottle",
        &[
            b("Bottle, every chance.", "Mid or feed. Mostly mid."),
            b("Bottle enjoyer.", "Runes were a lifestyle."),
            b("You bottled it.", "The item. Repeatedly."),
        ],
    ),
    (
        "radiance",
        "Radiance",
        &[
            b("Radiance, habitually.", "The creeps burned. Slowly."),
            b("You built Radiance.", "Your team waited. Patiently."),
            b("The burn aura, again.", "Fights? After Radiance."),
        ],
    ),
];

// People. `{name}` lines are skipped when no short readable name exists.

pub(super) const PEOPLE_POOLS: &[(&str, &[Beat])] = &[
    ("duo", DUO),
    ("cursed_duo", CURSED_DUO),
    ("nemesis", NEMESIS),
    ("punching_bag", PUNCHING_BAG),
];

const DUO: &[Beat] = &[
    b("You and {name}.", "Better than Tiny and Io."),
    b("{name} carried you.", "Or the other way around."),
    b("Your favorite teammate:", "{name}. Obviously."),
    b("Wins with {name}:", "Suspiciously many."),
    b("{name} on your team?", "Free win, basically."),
    b("Same team as {name}:", "The shuffler should worry."),
    b("Your best duo.", "The shuffler can't stop it."),
    b("Win together.", "Lose apart."),
    b("Lane partners in crime.", "The crime was winning."),
    b("Shuffled together.", "Won together. A lot."),
    b("A duo for the ages.", "Well, for this year."),
    b("When you two team up,", "It's just not fair."),
    b("The Io to their Tiny.", "Tossed straight into wins."),
];

const CURSED_DUO: &[Beat] = &[
    b("You and {name}.", "A black hole of MMR."),
    b("Same team as {name}?", "Pack it up."),
    b("{name} on your team.", "The enemy smiled."),
    b("Shuffled with {name}:", "Bad news, mostly."),
    b("Your cursed duo.", "Anti-synergy, perfected."),
    b("Shared suffering.", "Forged in losses."),
    b("The shuffler paired you two.", "As a warning."),
    b("Like a dual offlane.", "Bold. But why?"),
    b("Together, you two lost.", "A lot. Together."),
    b("Same team, same result.", "A loss, mostly."),
    b("Your combined powers:", "Cancel out, somehow."),
];

const NEMESIS: &[Beat] = &[
    b("{name}, rent-free.", "In your head, all year."),
    b("Lost to {name}:", "Often. Painfully often."),
    b("{name} on their team?", "Pain."),
    b("Your nemesis: {name}.", "BKB doesn't block that."),
    b("{name} counters you.", "Like Anti-Mage on Storm."),
    b("Your nemesis.", "The rivalry writes itself."),
    b("Every time they queued,", "You felt it."),
    b("Your personal Viper lane.", "No escape, all year."),
    b("You owe them a keyboard.", "At least one."),
    b("Radiant and Dire.", "Eternally opposed."),
    b("They had your number.", "And your MMR."),
    b("Matchmaking's favorite duo.", "Comedy duo, specifically."),
];

const PUNCHING_BAG: &[Beat] = &[
    b("{name} on their team?", "Free real estate."),
    b("You owned {name}.", "Everyone knows it."),
    b("{name} saw your name.", "And sighed."),
    b("Wins against {name}:", "Farmed like a jungle camp."),
    b("{name}, lane creep.", "They don't even know."),
    b("Your favorite opponent.", "Free real estate."),
    b("You farmed them.", "Like a jungle stack."),
    b("Not even close.", "Usually."),
    b("Their name in the lobby:", "Basically a free win."),
    b("Your personal creep wave.", "Respawning just for you."),
    b("They need a BKB.", "Just for you."),
];

// Side games. Extreme is the top of the server; Strong falls back to
// Notable. Gamba and Press F cards keep their meme as the punchline.

pub(super) const SIDE_POOLS: &[(&str, Tier, &[Beat])] = &[
    ("gamba_up", Extreme, GAMBA_UP_EXTREME),
    ("gamba_up", Notable, GAMBA_UP),
    ("gamba_down", Extreme, GAMBA_DOWN_EXTREME),
    ("gamba_down", Notable, GAMBA_DOWN),
    ("dig", Extreme, DIG_EXTREME),
    ("dig", Notable, DIG),
    ("cave_in", Notable, CAVE_IN),
    ("pet", Extreme, PET_EXTREME),
    ("pet", Notable, PET),
    ("pet_lost", Notable, PET_LOST),
    ("mafia", Extreme, MAFIA_EXTREME),
    ("mafia", Notable, MAFIA),
];

const GAMBA_UP_EXTREME: &[Beat] = &[
    b("Gamba, but profitable.", "Big stonks."),
    b("The house hates you.", "Stonks."),
    b("Professional degenerate.", "Stonks!"),
    b("The bookie tipped its hat.", "Stonks."),
    b("Printing Jopacoin.", "Big stonks."),
    b("The Oracle of Jopacoin.", "Stonks."),
];

const GAMBA_UP: &[Beat] = &[
    b("You cashed out.", "Stonks."),
    b("The house lost.", "Stonks."),
    b("Jopacoin, multiplied.", "Stonks!"),
    b("Bet smart, apparently.", "Stonks."),
    b("Your portfolio this year:", "Big stonks."),
    b("The bookie owes you.", "Stonks."),
    b("Net gamba result:", "Stonks!"),
];

const GAMBA_DOWN_EXTREME: &[Beat] = &[
    b("The bookie's best customer.", "Not stonks."),
    b("Subsidizing everyone.", "Not stonks."),
    b("Down bad, Jopacoin edition.", "Very not stonks."),
    b("All in. All gone.", "Not stonks!"),
    b("The economy thanks you.", "Not stonks."),
    b("Deep in the red.", "Very not stonks."),
];

const GAMBA_DOWN: &[Beat] = &[
    b("You bet. A lot.", "Not stonks."),
    b("The house thanks you.", "Not stonks."),
    b("Jopacoin, donated.", "Not stonks!"),
    b("Your portfolio this year:", "Very not stonks."),
    b("Net gamba result:", "Not stonks."),
    b("Double or nothing?", "Not stonks."),
    b("Bet high, landed low.", "Not stonks!"),
];

const DIG_EXTREME: &[Beat] = &[
    b("A tunnel for the ages.", "Roshan's pit looks shallow."),
    b("You dug. And dug.", "The earth wants a break."),
    b("Elite digger.", "Pickaxe, well worn."),
    b("Blocks dug: absurd.", "Tunnel vision, literally."),
    b("Dig rank: elite.", "Mole energy."),
    b("You went deep.", "Deeper than your MMR."),
];

const DIG: &[Beat] = &[
    b("You picked up a pickaxe.", "And kept swinging."),
    b("A respectable tunnel.", "Not deep. Respectable."),
    b("Dig, dig, dig.", "Between games, mostly."),
    b("Underground arc: started.", "Mole mode: casual."),
    b("You dug this year.", "The rocks noticed."),
    b("Tunnel progress:", "Steady. Mostly dirt."),
];

const CAVE_IN: &[Beat] = &[
    b("Your tunnel caved in.", "Press F."),
    b("The ceiling had opinions.", "F."),
    b("Rocks fell. Progress too.", "Press F."),
    b("The tunnel collapsed.", "Press F to pay respects."),
    b("Hard hat: not equipped.", "F."),
    b("Structural integrity: no.", "Press F."),
    b("Dig, dig, crumble.", "Press F to pay respects."),
];

const PET_EXTREME: &[Beat] = &[
    b("Pet keeper, elite tier.", "The pets are spoiled."),
    b("Your pets ate well.", "Better than your supports."),
    b("Pet spending: elite.", "Jopacoin well spent."),
    b("Your pet lives large.", "On your Jopacoin."),
    b("The pets love you.", "Allegedly. They can't talk."),
    b("Spoiled pets, confirmed.", "Zero regrets."),
];

const PET: &[Beat] = &[
    b("You kept a pet.", "It kept you company."),
    b("Pet duty: done.", "Mostly fed. Mostly."),
    b("Treats were purchased.", "Belly rubs: unconfirmed."),
    b("Your pet says thanks.", "In its own way. Loudly."),
    b("You raised a pet.", "Between games."),
    b("Pet keeper.", "Low effort, high love."),
];

const PET_LOST: &[Beat] = &[
    b("Your pet didn't make it.", "Press F."),
    b("A pet was lost.", "F."),
    b("Gone, but not forgotten.", "Press F to pay respects."),
    b("The pet graveyard grew.", "Press F."),
    b("Rest in peace, little one.", "F."),
    b("Pet lost this year.", "Press F."),
    b("One less mouth to feed.", "Press F."),
];

const MAFIA_EXTREME: &[Beat] = &[
    b("Mafia rank: top tier.", "Trust no one. Especially you."),
    b("You lied. Professionally.", "Town never stood a chance."),
    b("Poker face: legendary.", "Your votes, less so."),
    b("Night after night,", "Someone got got."),
    b("Mafia, all year.", "The town is suspicious."),
    b("Mafia: your second game.", "After Dota. Barely."),
];

const MAFIA: &[Beat] = &[
    b("You played some Mafia.", "Lied a little."),
    b("Town or Mafia?", "Depends who's asking."),
    b("Votes were cast.", "Some at the right person."),
    b("You survived some nights.", "Not all of them."),
    b("Suspicious? You?", "Never. Well, sometimes."),
    b("A Mafia regular.", "Lies told: some. Caught: few."),
];

/// A persona title (20 characters at most, it rides on the recap) and its
/// one-line subtitle.
type Persona = (&'static str, &'static str);

/// Personas keyed by the trait that defines the player.
pub(super) const PERSONAS: &[(&str, Direction, &[Persona])] = &[
    (
        "win_rate",
        Popoff,
        &[
            ("The Team Diff", "Balanced teams were never balanced."),
            ("Winners Queue", "Population: mostly you."),
        ],
    ),
    (
        "win_rate",
        Roast,
        &[
            ("Enemy MVP", "The other team's favorite teammate."),
            ("Free MMR", "Restocked every single game."),
        ],
    ),
    (
        "deaths_per_game",
        Roast,
        &[
            ("Fountain Regular", "The respawn timer knows you by name."),
            ("Space Creator", "The space was mostly the respawn timer."),
        ],
    ),
    (
        "deaths_per_game",
        Popoff,
        &[("The Unkillable", "Death simply couldn't find you.")],
    ),
    (
        "kills_per_game",
        Popoff,
        &[
            ("Kill Stealer", "Supports are still waiting for one."),
            ("The Executioner", "Violence was the answer. Every time."),
        ],
    ),
    (
        "best_game",
        Popoff,
        &[("Main Character", "One game they're still talking about.")],
    ),
    (
        "kill_loss",
        Roast,
        &[("KDA Player", "Great stats. Wrong scoreboard.")],
    ),
    (
        "gpm",
        Popoff,
        &[("Farm Simulator", "The creeps never stood a chance.")],
    ),
    (
        "gpm",
        Roast,
        &[("Pos 5 at Heart", "Gold is for other people.")],
    ),
    (
        "last_hits",
        Popoff,
        &[("CS Machine", "Click. Gold. Repeat.")],
    ),
    (
        "hero_damage",
        Popoff,
        &[("Glass Cannon", "Mostly cannon. Some glass.")],
    ),
    (
        "healing",
        Popoff,
        &[("Walking Fountain", "Healed everyone. Thanked by no one.")],
    ),
    (
        "stuns",
        Popoff,
        &[("Stun Lock Artist", "Enemies spent the year standing still.")],
    ),
    (
        "wards",
        Popoff,
        &[("The Lighthouse", "Bought the vision. Nobody looked.")],
    ),
    (
        "wards",
        Roast,
        &[("Fog Enjoyer", "Vision is overrated, apparently.")],
    ),
    (
        "stacks",
        Popoff,
        &[("Camp Stacker", "x:53, every minute, for nobody.")],
    ),
    (
        "runes",
        Popoff,
        &[("Rune Goblin", "Mid was not consulted.")],
    ),
    (
        "couriers",
        Popoff,
        &[("Courier Sniper", "No delivery was safe.")],
    ),
    (
        "pings",
        Roast,
        &[("Minimap DJ", "Ping, ping, ping, ping, ping.")],
    ),
    (
        "apm",
        Popoff,
        &[("Fast Hands", "Some of the clicks even mattered.")],
    ),
    (
        "throw",
        Roast,
        &[
            ("Professional Thrower", "Ahead is just a setup."),
            ("Lead Fumbler", "Every lead is temporary."),
        ],
    ),
    (
        "comeback",
        Popoff,
        &[("Comeback Kid", "Never trust a lead. Especially theirs.")],
    ),
    (
        "rapiers",
        Popoff,
        &[("Rapier Believer", "Bought the sword. It paid off.")],
    ),
    (
        "rapiers",
        Roast,
        &[("Rapier Donor", "Bought the sword. Lost the game.")],
    ),
    (
        "upset_win",
        Popoff,
        &[("The Underdog", "Calculated. Allegedly.")],
    ),
    (
        "upset_loss",
        Roast,
        &[("The Sure Thing", "Favored on paper. Paper lost.")],
    ),
    (
        "win_streak",
        Popoff,
        &[("The Heater", "Couldn't lose for a while there.")],
    ),
    (
        "loss_streak",
        Roast,
        &[("Tilt Queuer", "Lose, requeue, repeat.")],
    ),
    (
        "rating_swing",
        Popoff,
        &[("Rating Printer", "One game, one big payout.")],
    ),
    (
        "rating_swing",
        Roast,
        &[("Trapdoor Tester", "One game, one big drop.")],
    ),
    (
        "night_owl",
        Roast,
        &[
            ("The Late Lobby", "One more game, at any hour."),
            ("Night Creature", "The ancient falls best after midnight."),
        ],
    ),
    (
        "marathon_day",
        Roast,
        &[("One More Game", "A day is just a long session.")],
    ),
    (
        "hero_loyal",
        Popoff,
        &[("The One-Trick", "Why learn another hero?")],
    ),
    (
        "hero_loyal",
        Roast,
        &[("Loyal to a Fault", "The hero didn't love you back.")],
    ),
    (
        "hero_pool",
        Popoff,
        &[("Random Button", "Every hero, eventually.")],
    ),
    (
        "hero_pool",
        Roast,
        &[("Hero Puddle", "Other heroes exist. Allegedly.")],
    ),
    (
        "buybacks",
        Roast,
        &[("Buyback Enjoyer", "Gold is temporary. So is death.")],
    ),
    (
        "item",
        Roast,
        &[("The Usual Order", "The shop knows what you want.")],
    ),
];

pub(super) const FALLBACK_PERSONAS: &[Persona] = &[
    ("The Regular", "Showed up, queued, played."),
    ("Lobby Fixture", "Always somewhere in the shuffle."),
    ("Steady Hands", "Nothing extreme. Nothing broken."),
];
