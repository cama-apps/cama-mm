//! Item Updates: what a player's purchase logs say about their shopping.
//!
//! Only games with a purchase log count, and a log with a negative count is
//! no log at all. Item names and costs come from the caller's catalog; an
//! item the catalog does not know gets no line.

use std::collections::BTreeMap;

use cama_domain::formatting::{count_noun, format_grouped};
use cama_domain::wrapped_distinct::DistinctMatch;

use super::format_flavor;
use super::patch_support::{core, counted, own_games, percent, variant};
use super::personal_patch::BalanceDirection;

/// One purchasable item, supplied by the runtime from the game database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    /// The OpenDota purchase-log key, such as `black_king_bar`.
    pub key: String,
    pub name: String,
    pub cost: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemNote {
    /// The big item built in the most games.
    Signature,
    /// A big item whose games were mostly won or mostly lost.
    WinRate,
    /// The consumable bought furthest beyond the guild's habit.
    Consumable,
    /// A Rapier, a Midas habit, Gems, or a conspicuous absence.
    Oddity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemUpdate {
    pub key: String,
    pub name: String,
    pub note: ItemNote,
    pub line: String,
    /// Set for win-rate notes only.
    pub direction: Option<BalanceDirection>,
}

/// Games with a purchase log before any item tells a story.
const MIN_LOGGED_GAMES: usize = 5;
/// Catalog cost from which an item counts as a real build decision.
const REAL_ITEM_COST: i64 = 2_000;
const MIN_SIGNATURE_GAMES: usize = 3;
const MIN_WIN_RATE_GAMES: usize = 5;
/// Win rate with an item, in percent, that reads as a buff or a nerf.
const BUFF_RATE: usize = 65;
const NERF_RATE: usize = 35;
/// Percentage points an item's win rate must sit from the player's own.
const BASELINE_GAP: usize = 10;
const MIN_CONSUMABLES: i64 = 10;
/// Other players' logged games before their habits are a baseline.
const MIN_GUILD_LOGS: usize = 10;
const MIN_HABIT_GAMES: usize = 3;
/// Logged core games without the item before its absence is conspicuous.
const MIN_ABSENT_GAMES: usize = 15;

/// Expensive parts that are bought on the way to something else.
const COMPONENTS: &[&str] = &[
    "demon_edge",
    "eagle",
    "hyperstone",
    "mystic_staff",
    "platemail",
    "reaver",
    "relic",
    "soul_booster",
    "ultimate_orb",
];

/// Consumables that can carry a note: key, fallback name, lines.
const CONSUMABLES: &[(&str, &str, &[&str])] = &[
    (
        "tango",
        "Tango",
        &[
            "{n} purchased. Trees filed a complaint.",
            "{n} purchased. The forest has noticed.",
        ],
    ),
    (
        "tpscroll",
        "Town Portal Scroll",
        &[
            "{n} purchased. Arrival times varied.",
            "{n} purchased. Boots of Travel remain optional.",
        ],
    ),
    (
        "clarity",
        "Clarity",
        &[
            "{n} purchased. Mana problems persist.",
            "{n} purchased. Still out of mana.",
        ],
    ),
    (
        "flask",
        "Healing Salve",
        &[
            "{n} purchased. Laning remains hazardous.",
            "{n} purchased. Health remained a concern.",
        ],
    ),
    (
        "enchanted_mango",
        "Enchanted Mango",
        &[
            "{n} purchased. A balanced diet.",
            "{n} purchased. Fruit intake exceeds guidelines.",
        ],
    ),
    (
        "faerie_fire",
        "Faerie Fire",
        &[
            "{n} purchased. Each bought a little more time.",
            "{n} purchased. Emergency snacks approved.",
        ],
    ),
    (
        "smoke_of_deceit",
        "Smoke of Deceit",
        &[
            "{n} purchased. Ganks were attempted.",
            "{n} purchased. Somebody had a plan.",
        ],
    ),
    (
        "dust",
        "Dust of Appearance",
        &[
            "{n} purchased. Invisible heroes filed a complaint.",
            "{n} purchased. Nowhere left to hide.",
        ],
    ),
    (
        "ward_observer",
        "Observer Ward",
        &[
            "{n} purchased. The map has been seen.",
            "{n} purchased. Vision budget approved.",
        ],
    ),
    (
        "ward_sentry",
        "Sentry Ward",
        &[
            "{n} purchased. Enemy wards were inconvenienced.",
            "{n} purchased. Dewarding is a lifestyle.",
        ],
    ),
    (
        "tome_of_knowledge",
        "Tome of Knowledge",
        &[
            "{n} purchased. Studying continues.",
            "{n} purchased. Experience was for sale.",
        ],
    ),
    (
        "bottle",
        "Bottle",
        &[
            "{n} purchased. Runes sold separately.",
            "{n} purchased. Refills not included.",
        ],
    ),
];

const SIGNATURE: &[&str] = &[
    "Built in {g} of {n} {sample}.",
    "Built in {g} of {n} {sample}. No changes planned.",
    "Built in {g} of {n} {sample}. Considered core.",
];
const WIN_RATE_BUFF: &[&str] = &[
    "{p}% wins in {g} games with it.",
    "{p}% wins in {g} games with it. Buy it again.",
];
const WIN_RATE_NERF: &[&str] = &[
    "{p}% wins in {g} games with it.",
    "{p}% wins in {g} games with it. Reconsider.",
];
const RAPIER_ONE_WON: &[&str] = &[
    "{n} purchased in one game. The game was won.",
    "{n} purchased in one game. It worked.",
];
const RAPIER_ONE_LOST: &[&str] = &[
    "{n} purchased in one game. The game was lost.",
    "{n} purchased in one game. It did not help.",
];
const RAPIER_MANY: &[&str] = &[
    "{n} purchased in {g} games. {w} won.",
    "{n} purchased across {g} games. Won {w}.",
];
const ABSENT: &[&str] = &[
    "Purchased in 0 of {n} games as a core. Bold.",
    "Purchased in 0 of {n} core games. Working as intended.",
    "Purchased in 0 of {n} core games. Magic immunity: optional.",
];
const MIDAS: &[&str] = &[
    "Purchased in {g} of {n} {sample}. Won {w} of them.",
    "Purchased in {g} of {n} {sample}. {w} of them were won.",
];
const GEM: &[&str] = &[
    "{n} purchased. Final owner not recorded.",
    "{n} purchased. Custody was not tracked.",
];

struct Log<'a> {
    game: &'a DistinctMatch,
    counts: BTreeMap<&'a str, i64>,
}

impl Log<'_> {
    fn bought(&self, key: &str) -> i64 {
        self.counts.get(key).copied().unwrap_or(0)
    }
}

fn purchase_log(game: &DistinctMatch) -> Option<Log<'_>> {
    let mut counts = BTreeMap::new();
    for (key, count) in game.items.as_ref()? {
        if *count < 0 {
            return None;
        }
        *counts.entry(key.as_str()).or_insert(0) += count;
    }
    Some(Log { game, counts })
}

fn results(logs: &[&Log<'_>]) -> (usize, usize) {
    let games = logs.iter().map(|log| log.game).collect::<Vec<_>>();
    counted(&games, |game| game.won)
}

/// Up to four item notes for `player_id`, one of each kind and never the
/// same item twice: signature, win rate, consumable, oddity.
#[must_use]
pub fn item_updates(
    matches: &[DistinctMatch],
    player_id: i64,
    catalog: &[ItemInfo],
    seed: u64,
) -> Vec<ItemUpdate> {
    let games = own_games(matches, player_id);
    let logs = games
        .iter()
        .filter_map(|game| purchase_log(game))
        .collect::<Vec<_>>();
    if logs.len() < MIN_LOGGED_GAMES {
        return Vec::new();
    }
    let logged = logs.len().to_string();
    // "48 games" would claim the whole year when some games have no log.
    let sample = if logs.len() == games.len() {
        "games"
    } else {
        "parsed games"
    };
    let mut known: BTreeMap<&str, &ItemInfo> = BTreeMap::new();
    for item in catalog {
        known.entry(item.key.as_str()).or_insert(item);
    }
    let real = |key: &str| {
        known.get(key).copied().filter(|item| {
            item.cost >= REAL_ITEM_COST
                && !key.starts_with("recipe")
                && !COMPONENTS.contains(&key)
                && !CONSUMABLES.iter().any(|(held, _, _)| *held == key)
        })
    };
    let mut owners: BTreeMap<&str, Vec<&Log<'_>>> = BTreeMap::new();
    for log in &logs {
        for (key, count) in &log.counts {
            if *count > 0 {
                owners.entry(*key).or_default().push(log);
            }
        }
    }
    let total = |key: &str| logs.iter().map(|log| log.bought(key)).sum::<i64>();
    let write = |pool: &[&str], key: &str, variables: &[(&str, &str)]| {
        format_flavor(
            variant(seed, &format!("{}:{key}", pool[0]), pool),
            variables,
        )
    };
    let mut updates: Vec<ItemUpdate> = Vec::new();

    let signature = owners
        .iter()
        .filter_map(|(&key, with)| Some((real(key)?, with.len())))
        .filter(|(_, built)| *built >= MIN_SIGNATURE_GAMES)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.key.cmp(&a.0.key)));
    if let Some((item, built)) = signature {
        updates.push(ItemUpdate {
            key: item.key.clone(),
            name: item.name.clone(),
            note: ItemNote::Signature,
            line: write(
                SIGNATURE,
                &item.key,
                &[
                    ("g", &built.to_string()),
                    ("n", &logged),
                    ("sample", sample),
                ],
            ),
            direction: None,
        });
    }

    let (all_wins, all_known) = results(&logs.iter().collect::<Vec<_>>());
    let win_rate = owners
        .iter()
        .filter(|(key, _)| updates.iter().all(|held| held.key != **key))
        .filter_map(|(&key, with)| {
            let item = real(key)?;
            let (wins, played) = results(with);
            let lopsided = wins * 100 >= played * BUFF_RATE || wins * 100 <= played * NERF_RATE;
            // Cross-multiplied against the player's own logged win rate.
            let apart = (wins * all_known).abs_diff(all_wins * played) * 100
                >= BASELINE_GAP * played * all_known;
            (played >= MIN_WIN_RATE_GAMES && lopsided && apart).then_some((item, wins, played))
        })
        .max_by(|a, b| {
            let extreme = |(_, wins, played): &(&ItemInfo, usize, usize)| {
                (2 * wins).abs_diff(*played) as f64 / *played as f64
            };
            extreme(a)
                .total_cmp(&extreme(b))
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| b.0.key.cmp(&a.0.key))
        });
    if let Some((item, wins, played)) = win_rate {
        let buff = wins * 2 > played;
        updates.push(ItemUpdate {
            key: item.key.clone(),
            name: item.name.clone(),
            note: ItemNote::WinRate,
            line: write(
                if buff { WIN_RATE_BUFF } else { WIN_RATE_NERF },
                &item.key,
                &[
                    ("p", &percent(wins, played).to_string()),
                    ("g", &played.to_string()),
                ],
            ),
            direction: Some(if buff {
                BalanceDirection::Buff
            } else {
                BalanceDirection::Nerf
            }),
        });
    }

    let guild_logs = matches
        .iter()
        .filter(|game| game.discord_id != player_id)
        .filter_map(purchase_log)
        .collect::<Vec<_>>();
    let consumable = CONSUMABLES
        .iter()
        .filter_map(|&(key, fallback, lines)| {
            let bought = total(key);
            if bought < MIN_CONSUMABLES {
                return None;
            }
            // Against the guild's own habit when there is one to compare
            // with; otherwise the biggest pile wins.
            let standout = if guild_logs.len() >= MIN_GUILD_LOGS {
                let habit = guild_logs.iter().map(|log| log.bought(key)).sum::<i64>() as f64
                    / guild_logs.len() as f64;
                bought as f64 / logs.len() as f64 / habit.max(0.05)
            } else {
                bought as f64
            };
            Some((standout, key, fallback, lines, bought))
        })
        // The first listed consumable wins a tie.
        .reduce(|best, next| if next.0 > best.0 { next } else { best });
    if let Some((_, key, fallback, lines, bought)) = consumable {
        updates.push(ItemUpdate {
            key: key.to_owned(),
            name: known
                .get(key)
                .map_or(fallback, |item| item.name.as_str())
                .to_owned(),
            note: ItemNote::Consumable,
            line: write(lines, key, &[("n", &format_grouped(bought))]),
            direction: None,
        });
    }

    let fresh = |key: &str| {
        known
            .get(key)
            .copied()
            .filter(|_| updates.iter().all(|held| held.key != key))
    };
    let with = |key: &str| owners.get(key).map_or(&[][..], Vec::as_slice);
    let oddity = None
        .or_else(|| {
            let (item, bought, games) = (fresh("rapier")?, total("rapier"), with("rapier"));
            let (wins, decided) = results(games);
            let count = format_grouped(bought);
            let line = match (games.len(), decided == games.len()) {
                (0, _) => return None,
                (played, false) => format!(
                    "{count} purchased in {}.",
                    count_noun(played as i64, "game", "games")
                ),
                (1, true) => write(
                    if wins == 1 {
                        RAPIER_ONE_WON
                    } else {
                        RAPIER_ONE_LOST
                    },
                    "rapier",
                    &[("n", &count)],
                ),
                (played, true) => write(
                    RAPIER_MANY,
                    "rapier",
                    &[
                        ("n", &count),
                        ("g", &played.to_string()),
                        ("w", &wins.to_string()),
                    ],
                ),
            };
            Some((item, line))
        })
        .or_else(|| {
            let item = fresh("black_king_bar")?;
            let cores = logs
                .iter()
                .filter(|log| core(log.game) == Some(true))
                .collect::<Vec<_>>();
            let never = cores.iter().all(|log| log.bought("black_king_bar") == 0);
            (cores.len() >= MIN_ABSENT_GAMES && never).then(|| {
                let line = write(ABSENT, "black_king_bar", &[("n", &cores.len().to_string())]);
                (item, line)
            })
        })
        .or_else(|| {
            let (item, games) = (fresh("hand_of_midas")?, with("hand_of_midas"));
            if games.len() < MIN_HABIT_GAMES {
                return None;
            }
            let (wins, decided) = results(games);
            let bought = games.len().to_string();
            let line = if decided == games.len() {
                write(
                    MIDAS,
                    "hand_of_midas",
                    &[
                        ("g", &bought),
                        ("n", &logged),
                        ("sample", sample),
                        ("w", &wins.to_string()),
                    ],
                )
            } else {
                format!("Purchased in {bought} of {logged} {sample}.")
            };
            Some((item, line))
        })
        .or_else(|| {
            let (item, bought) = (fresh("gem")?, total("gem"));
            (bought >= MIN_HABIT_GAMES as i64)
                .then(|| (item, write(GEM, "gem", &[("n", &format_grouped(bought))])))
        });
    if let Some((item, line)) = oddity {
        updates.push(ItemUpdate {
            key: item.key.clone(),
            name: item.name.clone(),
            note: ItemNote::Oddity,
            line,
            direction: None,
        });
    }
    updates
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn catalog() -> Vec<ItemInfo> {
        [
            ("blink", "Blink Dagger", 2250),
            ("black_king_bar", "Black King Bar", 4050),
            ("bfury", "Battle Fury", 4100),
            ("rapier", "Divine Rapier", 5600),
            ("hand_of_midas", "Hand of Midas", 2200),
            ("gem", "Gem of True Sight", 900),
            ("magic_wand", "Magic Wand", 450),
            ("relic", "Sacred Relic", 3400),
            ("recipe_black_king_bar", "Black King Bar Recipe", 2250),
            ("tpscroll", "Town Portal Scroll", 100),
        ]
        .into_iter()
        .map(|(key, name, cost)| ItemInfo {
            key: key.to_owned(),
            name: name.to_owned(),
            cost,
        })
        .collect()
    }

    fn game(id: i64, items: &[(&str, i64)], won: bool) -> DistinctMatch {
        DistinctMatch {
            match_id: id,
            discord_id: 7,
            finished_at: Some(id * 86_400),
            won: Some(won),
            items: Some(
                items
                    .iter()
                    .map(|(key, count)| ((*key).to_owned(), *count))
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn note(updates: &[ItemUpdate], note: ItemNote) -> Option<&ItemUpdate> {
        updates.iter().find(|update| update.note == note)
    }

    #[test]
    fn nothing_is_said_without_five_purchase_logs() {
        let mut rows = (1..=5)
            .map(|id| game(id, &[("blink", 1), ("tango", 9)], true))
            .collect::<Vec<_>>();
        assert!(!item_updates(&rows, 7, &catalog(), 0).is_empty());
        // No log is unknown, not an empty shopping list.
        rows[0].items = None;
        assert!(item_updates(&rows, 7, &catalog(), 0).is_empty());
        // A negative count spoils the whole log.
        rows[0].items = Some(vec![("blink".to_owned(), 1), ("tango".to_owned(), -1)]);
        assert!(item_updates(&rows, 7, &catalog(), 0).is_empty());
        rows[0].items = Some(Vec::new());
        assert!(!item_updates(&rows, 7, &catalog(), 0).is_empty());
        assert!(item_updates(&rows, 8, &catalog(), 0).is_empty());
    }

    #[test]
    fn signature_is_the_most_built_real_item() {
        // Cheap items, recipes, components, consumables and anything the
        // catalog does not know are out, however often they were bought.
        let everywhere = [
            ("magic_wand", 1),
            ("recipe_black_king_bar", 1),
            ("relic", 1),
            ("tpscroll", 4),
            ("mystery_item", 1),
        ];
        let mut rows = (1..=6)
            .map(|id| {
                let mut items = everywhere.to_vec();
                if id <= 4 {
                    items.push(("blink", 1));
                }
                if id <= 3 {
                    items.push(("bfury", 2));
                }
                game(id, &items, id % 2 == 0)
            })
            .collect::<Vec<_>>();
        let updates = item_updates(&rows, 7, &catalog(), 0);
        let signature = note(&updates, ItemNote::Signature).expect("signature");
        assert_eq!(
            (signature.key.as_str(), signature.name.as_str()),
            ("blink", "Blink Dagger")
        );
        assert!(signature.line.starts_with("Built in 4 of 6 games."));
        assert_eq!(signature.direction, None);

        // Games without a log shrink the sample, and the line says so.
        rows.push(DistinctMatch {
            match_id: 7,
            discord_id: 7,
            ..Default::default()
        });
        let updates = item_updates(&rows, 7, &catalog(), 0);
        assert!(
            note(&updates, ItemNote::Signature)
                .unwrap()
                .line
                .starts_with("Built in 4 of 6 parsed games.")
        );

        // Two games is a purchase, not a signature.
        let rows = (1..=6)
            .map(|id| game(id, &[("blink", i64::from(id <= 2))], true))
            .collect::<Vec<_>>();
        assert!(item_updates(&rows, 7, &catalog(), 0).is_empty());
    }

    #[test]
    fn win_rate_note_needs_five_games_a_lopsided_rate_and_a_gap_from_the_baseline() {
        // Blink in all ten games; Battle Fury in five, four of them won.
        let rows = |fury_wins: i64, other_wins: i64| {
            (1..=10)
                .map(|id| {
                    if id <= 5 {
                        game(id, &[("blink", 1), ("bfury", 1)], id <= fury_wins)
                    } else {
                        game(id, &[("blink", 1)], id - 5 <= other_wins)
                    }
                })
                .collect::<Vec<_>>()
        };
        let updates = item_updates(&rows(4, 1), 7, &catalog(), 0);
        let rate = note(&updates, ItemNote::WinRate).expect("win rate");
        assert_eq!(rate.key, "bfury");
        assert!(rate.line.starts_with("80% wins in 5 games with it."));
        assert_eq!(rate.direction, Some(BalanceDirection::Buff));
        assert_eq!(note(&updates, ItemNote::Signature).unwrap().key, "blink");

        let updates = item_updates(&rows(1, 4), 7, &catalog(), 0);
        let rate = note(&updates, ItemNote::WinRate).expect("win rate");
        assert!(rate.line.starts_with("20% wins in 5 games with it."));
        assert_eq!(rate.direction, Some(BalanceDirection::Nerf));

        // 60% is not lopsided.
        assert!(
            note(
                &item_updates(&rows(3, 1), 7, &catalog(), 0),
                ItemNote::WinRate
            )
            .is_none()
        );
        // 80% with the item means little when 80% is the player's norm.
        assert!(
            note(
                &item_updates(&rows(4, 4), 7, &catalog(), 0),
                ItemNote::WinRate
            )
            .is_none()
        );
        // Four known results are not a sample.
        let mut thin = rows(4, 1);
        thin[4].won = None;
        assert!(note(&item_updates(&thin, 7, &catalog(), 0), ItemNote::WinRate).is_none());
    }

    #[test]
    fn consumable_note_picks_the_biggest_pile_or_the_one_furthest_from_the_guild() {
        let mut rows = (1..=5)
            .map(|id| game(id, &[("tpscroll", 6), ("dust", 2), ("tango", 1)], true))
            .collect::<Vec<_>>();
        let updates = item_updates(&rows, 7, &catalog(), 0);
        let consumable = note(&updates, ItemNote::Consumable).expect("consumable");
        assert_eq!(consumable.key, "tpscroll");
        assert!(consumable.line.starts_with("30 purchased."));

        // Everyone buys scrolls; nobody else buys ten Dust.
        rows.extend((1..=10).map(|id| DistinctMatch {
            discord_id: 8,
            ..game(id, &[("tpscroll", 6)], true)
        }));
        let updates = item_updates(&rows, 7, &catalog(), 0);
        let consumable = note(&updates, ItemNote::Consumable).expect("consumable");
        // Dust is not in the catalog, so the built-in name stands in.
        assert_eq!(
            (consumable.key.as_str(), consumable.name.as_str()),
            ("dust", "Dust of Appearance")
        );
        assert!(consumable.line.starts_with("10 purchased."));

        // Nine of anything is not a habit.
        let rows = (1..=9)
            .map(|id| game(id, &[("tango", 1)], true))
            .collect::<Vec<_>>();
        assert!(item_updates(&rows, 7, &catalog(), 0).is_empty());
    }

    #[test]
    fn rapier_note_reports_only_what_the_results_prove() {
        let rows = |results: [Option<bool>; 2]| {
            (1..=6)
                .map(|id| {
                    let mut row = game(id, &[("rapier", if id <= 2 { 2 } else { 0 })], true);
                    if id <= 2 {
                        row.won = results[id as usize - 1];
                    }
                    row
                })
                .collect::<Vec<_>>()
        };
        let line = |rows: &[DistinctMatch], seed| {
            let updates = item_updates(rows, 7, &catalog(), seed);
            let oddity = note(&updates, ItemNote::Oddity).expect("oddity");
            assert_eq!(oddity.name, "Divine Rapier");
            oddity.line.clone()
        };
        let both = (0..16)
            .map(|seed| line(&rows([Some(true), Some(false)]), seed))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            both,
            BTreeSet::from([
                "4 purchased in 2 games. 1 won.".to_owned(),
                "4 purchased across 2 games. Won 1.".to_owned(),
            ])
        );
        assert_eq!(
            line(&rows([Some(true), None]), 0),
            "4 purchased in 2 games."
        );
        let mut single = rows([Some(false), Some(true)]);
        single[1].items = Some(Vec::new());
        assert!(line(&single, 0).starts_with("2 purchased in one game."));
        assert!(
            RAPIER_ONE_LOST
                .iter()
                .any(|template| { format_flavor(template, &[("n", "2")]) == line(&single, 0) })
        );
    }

    #[test]
    fn absent_bkb_needs_fifteen_logged_core_games() {
        let rows = |cores: i64| {
            (1..=20)
                .map(|id| DistinctMatch {
                    position: Some(if id <= cores { 2 } else { 5 }),
                    ..game(id, &[("magic_wand", 1)], true)
                })
                .collect::<Vec<_>>()
        };
        let updates = item_updates(&rows(15), 7, &catalog(), 0);
        let oddity = note(&updates, ItemNote::Oddity).expect("oddity");
        assert_eq!(oddity.key, "black_king_bar");
        assert!(oddity.line.starts_with("Purchased in 0 of 15 "));
        assert!(item_updates(&rows(14), 7, &catalog(), 0).is_empty());
        // One purchase as a core and the absence is gone.
        let mut bought = rows(15);
        bought[0].items = Some(vec![("black_king_bar".to_owned(), 1)]);
        assert!(note(&item_updates(&bought, 7, &catalog(), 0), ItemNote::Oddity).is_none());
        // Unknown positions are not core games.
        let mut unknown = rows(15);
        unknown[0].position = None;
        assert!(item_updates(&unknown, 7, &catalog(), 0).is_empty());
        // Without a name there is no line.
        let nameless = catalog()
            .into_iter()
            .filter(|item| item.key != "black_king_bar")
            .collect::<Vec<_>>();
        assert!(item_updates(&rows(15), 7, &nameless, 0).is_empty());
    }

    #[test]
    fn midas_and_gem_habits_need_three_and_yield_to_a_rapier() {
        // Blink in every game keeps the signature away from the oddities.
        let rows = |items: &[(&'static str, i64)]| {
            (1..=6)
                .map(|id| {
                    let mut bought = vec![("blink", 1)];
                    if id <= 3 {
                        bought.extend_from_slice(items);
                    }
                    game(id, &bought, id != 1)
                })
                .collect::<Vec<_>>()
        };
        let oddity = |rows: &[DistinctMatch]| {
            note(&item_updates(rows, 7, &catalog(), 0), ItemNote::Oddity).cloned()
        };
        let midas = oddity(&rows(&[("hand_of_midas", 1)])).expect("midas");
        assert_eq!(midas.key, "hand_of_midas");
        assert!(midas.line.starts_with("Purchased in 3 of 6 games."));
        assert!(midas.line.contains('2'));
        let gem = oddity(&rows(&[("gem", 1)])).expect("gem");
        assert_eq!(gem.name, "Gem of True Sight");
        assert!(gem.line.starts_with("3 purchased."));
        assert!(oddity(&rows(&[("gem", 1)])[1..]).is_none());
        let both = oddity(&rows(&[("gem", 1), ("hand_of_midas", 1), ("rapier", 1)])).unwrap();
        assert_eq!(both.key, "rapier");
        let unknown = rows(&[("hand_of_midas", 1)])
            .into_iter()
            .map(|row| DistinctMatch { won: None, ..row })
            .collect::<Vec<_>>();
        assert_eq!(oddity(&unknown).unwrap().line, "Purchased in 3 of 6 games.");
    }

    fn busy_year() -> Vec<DistinctMatch> {
        (1..=20)
            .map(|id| {
                let mut items = vec![("blink", 1), ("tango", 3)];
                if id <= 6 {
                    items.push(("bfury", 1));
                }
                if id == 20 {
                    items.push(("rapier", 1));
                }
                game(id, &items, id <= 6 || id % 4 == 0)
            })
            .collect()
    }

    #[test]
    fn notes_are_capped_distinct_ordered_and_stable_under_reordering() {
        let mut rows = busy_year();
        let updates = item_updates(&rows, 7, &catalog(), 3);
        assert_eq!(
            updates
                .iter()
                .map(|update| (update.note, update.key.as_str()))
                .collect::<Vec<_>>(),
            [
                (ItemNote::Signature, "blink"),
                (ItemNote::WinRate, "bfury"),
                (ItemNote::Consumable, "tango"),
                (ItemNote::Oddity, "rapier"),
            ]
        );
        rows.reverse();
        let mut shuffled = catalog();
        shuffled.reverse();
        assert_eq!(item_updates(&rows, 7, &shuffled, 3), updates);

        // A Rapier in most games is the signature and is not repeated.
        let rapiers = (1..=6)
            .map(|id| game(id, &[("rapier", 1)], true))
            .collect::<Vec<_>>();
        let updates = item_updates(&rapiers, 7, &catalog(), 3);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].note, ItemNote::Signature);
    }

    #[test]
    fn wording_varies_by_seed_and_every_variant_stays_short() {
        let rows = busy_year();
        let wordings = (0..32)
            .map(|seed| format!("{:?}", item_updates(&rows, 7, &catalog(), seed)))
            .collect::<BTreeSet<_>>();
        assert!(wordings.len() > 1);
        assert_eq!(
            item_updates(&rows, 7, &catalog(), 5),
            item_updates(&rows, 7, &catalog(), 5)
        );

        let wide = [
            ("n", "12,345"),
            ("g", "1234"),
            ("w", "1234"),
            ("p", "100"),
            ("sample", "parsed games"),
        ];
        let pools = CONSUMABLES.iter().map(|(_, _, lines)| *lines).chain([
            SIGNATURE,
            WIN_RATE_BUFF,
            WIN_RATE_NERF,
            RAPIER_ONE_WON,
            RAPIER_ONE_LOST,
            RAPIER_MANY,
            ABSENT,
            MIDAS,
            GEM,
        ]);
        for pool in pools {
            assert!(pool.len() >= 2);
            for template in pool {
                let line = format_flavor(template, &wide);
                assert!(!line.contains('{'), "{template}");
                assert!(line.chars().count() <= 64, "{line}");
                assert!(line.ends_with('.'), "{line}");
            }
        }
        assert_eq!(CONSUMABLES.len(), 12);
    }
}
