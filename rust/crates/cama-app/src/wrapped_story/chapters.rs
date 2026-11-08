//! Cards for the parts of a year that happen outside the match queue: the
//! dig, the Camagotchi, gamba, and Mafia, plus the month-by-month arc and the
//! activity summary for the year.
//!
//! Every card is backed by a recorded number. Copy is chosen from the numbers
//! and a per-player seed, so a story reads the same every time it is replayed.
//! Side-game cards lead with a roast or popoff from `roast_pools`.

use cama_domain::formatting::{format_grouped, format_grouped_signed};
use cama_domain::wrapped_activity::{
    DigYear, GambaYear, GambleKind, MafiaYear, PetYear, ServerRank,
};
use cama_domain::wrapped_distinct::Tier;

use super::roast_pools::side_game_copy;

/// How a card arranges its content. Mirrors the renderer's layouts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardStyle {
    BigNumber,
    Tiles,
    Spotlight,
    Bars,
    Curve,
    Persona,
}

/// The color story of a card. Mirrors the renderer's themes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardMood {
    Months,
    GambaUp,
    GambaDown,
    Dig,
    DigDeep,
    Pet,
    PetGrave,
    Mafia,
    MafiaRed,
    Persona,
}

/// Game art a card wants beside its numbers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CardArt {
    None,
    BossVictory(String),
    BossDefeat(String),
    BossEncounter(String),
    ActivePet,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChapterCard {
    pub kind: &'static str,
    pub style: CardStyle,
    pub mood: CardMood,
    pub art: CardArt,
    pub title: String,
    pub headline: String,
    pub stat_value: String,
    pub stat_label: String,
    pub badge: String,
    pub lines: Vec<String>,
    /// `(primary, secondary)` pairs for tiles and lists.
    pub rows: Vec<(String, String)>,
    /// Values for bar and curve cards.
    pub series: Vec<f64>,
}

impl ChapterCard {
    fn new(kind: &'static str, style: CardStyle, mood: CardMood, title: &str) -> Self {
        Self {
            kind,
            style,
            mood,
            art: CardArt::None,
            title: title.to_owned(),
            headline: String::new(),
            stat_value: String::new(),
            stat_label: String::new(),
            badge: String::new(),
            lines: Vec::new(),
            rows: Vec::new(),
            series: Vec::new(),
        }
    }
}

/// Looks up a readable display name; `None` means the card should not name
/// anyone rather than show an ID or a placeholder.
pub type NameLookup<'a> = &'a dyn Fn(i64) -> Option<String>;

fn plural(count: i64, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        format_grouped(count),
        if count == 1 { one } else { many }
    )
}

/// Pick one line of copy for this player without randomness at render time.
fn pick<'a>(seed: u64, salt: u64, pool: &[&'a str]) -> &'a str {
    let mut mixed = seed ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    mixed ^= mixed >> 33;
    mixed = mixed.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    mixed ^= mixed >> 33;
    pool[(mixed % pool.len() as u64) as usize]
}

/// A stable per-player, per-year seed for copy choices.
#[must_use]
pub fn story_seed(discord_id: i64, year: i32) -> u64 {
    (discord_id as u64).rotate_left(17) ^ u64::from(year.unsigned_abs())
}

/// How loud a side-game joke can be: the top 5% of the server gets the
/// extreme lines, everyone else the everyday ones.
#[must_use]
pub fn side_tier(rank: Option<ServerRank>) -> Tier {
    match rank.and_then(ServerRank::ahead_of_percent) {
        Some(percent) if percent >= 95.0 => Tier::Extreme,
        _ => Tier::Notable,
    }
}

/// A side game's setup and punchline as one line of copy.
fn side_joke(key: &str, tier: Tier, seed: u64) -> String {
    let beat = side_game_copy(key, tier, seed);
    let mut punch = beat.punch.chars();
    match (beat.setup.ends_with(','), punch.next()) {
        (true, Some(first)) => format!("{} {}{}", beat.setup, first.to_lowercase(), punch.as_str()),
        _ => format!("{} {}", beat.setup, beat.punch),
    }
}

/// "More than 92% of the server" style comparisons, only when they flatter.
fn rank_badge(rank: Option<ServerRank>, verb: &str, champion: &str) -> String {
    let Some(rank) = rank else {
        return String::new();
    };
    match rank.ahead_of_percent() {
        Some(_) if rank.rank == 1 => format!("#1 {champion} on the server"),
        Some(percent) if percent >= 50.0 => format!("{verb} {percent:.0}% of the server"),
        _ => String::new(),
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Games per calendar month from `YYYY-MM-DD…` match dates.
#[must_use]
pub fn months_card(match_dates: &[&str], seed: u64) -> Option<ChapterCard> {
    let mut games = [0_i64; 12];
    for date in match_dates {
        if let Some(month) = date
            .get(5..7)
            .and_then(|month| month.parse::<usize>().ok())
            .filter(|month| (1..=12).contains(month))
        {
            games[month - 1] += 1;
        }
    }
    let active = games.iter().filter(|count| **count > 0).count();
    if active < 2 {
        return None;
    }
    let last = games.iter().rposition(|count| *count > 0)?;
    let shown = &games[..=last];
    let peak = (0..shown.len()).max_by_key(|index| (shown[*index], usize::MAX - *index))?;
    let first = games.iter().position(|count| *count > 0)?;
    let quiet = (first..=last).min_by_key(|index| (shown[*index], *index))?;
    let mut card = ChapterCard::new(
        "story_months",
        CardStyle::Bars,
        CardMood::Months,
        "Your Dota year",
    );
    card.headline = pick(
        seed,
        1,
        &[
            "You played most in {month}.",
            "{month} was your busiest month.",
            "Your most active month was {month}.",
        ],
    )
    .replace("{month}", MONTHS[peak]);
    card.stat_value = format_grouped(shown[peak]);
    card.stat_label = if shown[quiet] == 0 {
        format!("games in {}. No games in {}.", MONTHS[peak], MONTHS[quiet])
    } else if quiet != peak && shown[quiet] * 3 <= shown[peak] {
        format!(
            "games in {}. You played less in {}.",
            MONTHS[peak], MONTHS[quiet]
        )
    } else {
        format!("games in {}, your busiest month.", MONTHS[peak])
    };
    card.rows = (0..shown.len())
        .map(|index| (MONTHS[index][..1].to_owned(), String::new()))
        .collect();
    card.series = shown.iter().map(|count| *count as f64).collect();
    Some(card)
}

struct GambleSummary {
    count: i64,
    bets: i64,
    net: i64,
    peak: i64,
    trough: i64,
    best: i64,
    worst: i64,
    losing_streak: i64,
    max_leverage: i64,
    double_or_nothing: i64,
    curve: Vec<f64>,
}

fn summarize(year: &GambaYear) -> GambleSummary {
    let mut summary = GambleSummary {
        count: year.events.len() as i64,
        bets: 0,
        net: 0,
        peak: 0,
        trough: 0,
        best: 0,
        worst: 0,
        losing_streak: 0,
        max_leverage: 1,
        double_or_nothing: 0,
        curve: Vec::with_capacity(year.events.len() + 1),
    };
    summary.curve.push(0.0);
    let mut streak = 0;
    for event in &year.events {
        summary.net = summary.net.saturating_add(event.profit);
        summary.peak = summary.peak.max(summary.net);
        summary.trough = summary.trough.min(summary.net);
        summary.best = summary.best.max(event.profit);
        summary.worst = summary.worst.min(event.profit);
        streak = if event.profit < 0 { streak + 1 } else { 0 };
        summary.losing_streak = summary.losing_streak.max(streak);
        match event.kind {
            GambleKind::Bet => {
                summary.bets += 1;
                summary.max_leverage = summary.max_leverage.max(event.leverage);
            }
            GambleKind::DoubleOrNothing => summary.double_or_nothing += 1,
            GambleKind::Wheel => {}
        }
        summary.curve.push(summary.net as f64);
    }
    summary
}

/// The gamba chapter: a profit curve and a degen report.
#[must_use]
pub fn gamba_cards(
    year: &GambaYear,
    degen: Option<(i64, &str)>,
    bankruptcies: i64,
    names: NameLookup<'_>,
    seed: u64,
) -> Vec<ChapterCard> {
    let summary = summarize(year);
    if summary.count < 3 {
        return Vec::new();
    }
    let up = summary.net >= 0;
    let mut curve = ChapterCard::new(
        "chart_gamba",
        CardStyle::Curve,
        if up {
            CardMood::GambaUp
        } else {
            CardMood::GambaDown
        },
        "Gamba",
    );
    curve.headline = if summary.net > 0 {
        pick(
            seed,
            2,
            &[
                "You finished with a profit.",
                "Your net result was a profit.",
                "You came out ahead.",
            ],
        )
    } else if summary.net < 0 {
        pick(
            seed,
            2,
            &[
                "You finished with a loss.",
                "Your net result was a loss.",
                "You ended the year down.",
            ],
        )
    } else {
        "You broke even."
    }
    .to_owned();
    curve.stat_value = format!("{} JC", format_grouped_signed(summary.net));
    curve.stat_label = format!("across {}", plural(summary.count, "gamble", "gambles"));
    curve.badge = rank_badge(year.rank, "Wagered more than", "bettor");
    if summary.peak > 0 && summary.net < summary.peak {
        curve.lines.push(format!(
            "Highest net profit: {} JC.",
            format_grouped_signed(summary.peak)
        ));
    } else if summary.trough < 0 && summary.net > 0 {
        curve.lines.push(format!(
            "Largest net loss: {} JC.",
            format_grouped(-summary.trough)
        ));
    }
    curve.series = summary.curve.clone();

    let mut report = ChapterCard::new(
        "story_gamba",
        CardStyle::Tiles,
        if up {
            CardMood::GambaUp
        } else {
            CardMood::GambaDown
        },
        "Gamba results",
    );
    report.headline = "Your biggest wins and losses.".to_owned();
    if let Some((score, _)) = degen {
        report
            .rows
            .push((score.to_string(), "degen score (lifetime)".to_owned()));
    }
    if summary.best > 0 {
        report.rows.push((
            format_grouped_signed(summary.best),
            "biggest win (JC)".to_owned(),
        ));
    }
    if summary.worst < 0 {
        report.rows.push((
            format_grouped(summary.worst),
            "biggest loss (JC)".to_owned(),
        ));
    }
    if year.wheel.spins > 0 {
        report
            .rows
            .push((format_grouped(year.wheel.spins), "wheel spins".to_owned()));
    }
    if summary.max_leverage > 1 {
        report.rows.push((
            format!("{}x", summary.max_leverage),
            "highest leverage".to_owned(),
        ));
    }
    if summary.double_or_nothing > 0 {
        report.rows.push((
            format_grouped(summary.double_or_nothing),
            "double-or-nothing games".to_owned(),
        ));
    }
    if bankruptcies > 0 {
        report.rows.push((
            format_grouped(bankruptcies),
            if bankruptcies == 1 {
                "bankruptcy, lifetime"
            } else {
                "bankruptcies, lifetime"
            }
            .to_owned(),
        ));
    }
    report.rows.truncate(6);
    if year.wheel.bankrupts > 0 {
        report.lines.push(format!(
            "The wheel bankrupted you {}.",
            plural(year.wheel.bankrupts, "time", "times")
        ));
    } else if summary.losing_streak >= 5 {
        report.lines.push(format!(
            "Longest losing streak: {} gambles.",
            summary.losing_streak
        ));
    }
    if let Some(friend) = year
        .tips
        .top_recipient
        .and_then(|tally| Some((names(tally.discord_id)?, tally.count)))
    {
        report.lines.push(format!(
            "You tipped {} the most: {} JC.",
            friend.0,
            format_grouped(friend.1)
        ));
    }
    let mut cards = vec![curve];
    if report.rows.len() >= 2 {
        cards.push(report);
    }
    cards
}

/// The dig's loudest number: blocks of progress, or the digs themselves when
/// cave-ins ate all of it. Returns the number and what it counts.
#[must_use]
pub fn dig_headline_number(year: &DigYear) -> (String, &'static str) {
    if year.blocks_dug > 0 {
        (format_grouped(year.blocks_dug), "blocks dug")
    } else {
        (format_grouped(year.digs), "digs")
    }
}

/// What the dig chapter needs to know about the game's own content.
pub struct DigContext<'a> {
    pub boss_name: &'a dyn Fn(&str) -> Option<String>,
    /// The layer at the player's deepest point this year.
    pub deepest_layer: Option<&'a str>,
    /// Whether that layer counts as the deep end of the mine.
    pub deep: bool,
}

#[must_use]
pub fn dig_cards(
    year: &DigYear,
    context: &DigContext<'_>,
    names: NameLookup<'_>,
    seed: u64,
) -> Vec<ChapterCard> {
    if year.digs < 5 {
        return Vec::new();
    }
    let mood = if context.deep {
        CardMood::DigDeep
    } else {
        CardMood::Dig
    };
    let mut cards = Vec::new();
    let mut dig = ChapterCard::new("story_dig", CardStyle::BigNumber, mood, "Dig");
    dig.headline = side_joke("dig", side_tier(year.rank), seed);
    dig.lines.push(dig.headline.clone());
    let (value, _) = dig_headline_number(year);
    dig.stat_value = value;
    dig.stat_label = if year.blocks_dug > 0 {
        match context.deepest_layer {
            Some(layer) => format!(
                "blocks dug. Deepest: {} in {layer}.",
                format_grouped(year.deepest)
            ),
            None => format!("blocks dug. Deepest: {}.", format_grouped(year.deepest)),
        }
    } else {
        "digs".to_owned()
    };
    dig.badge = rank_badge(year.rank, "Dug more than", "digger");
    if year.cave_ins >= 3 {
        dig.lines.push(format!(
            "You had {} cave-ins.",
            format_grouped(year.cave_ins)
        ));
    }
    if year.artifacts > 0 {
        let found = plural(year.artifacts, "artifact", "artifacts");
        dig.lines.push(match year.first_finds {
            0 => format!("Unearthed {found}."),
            _ if year.artifacts == 1 => format!("Unearthed {found} before anyone else."),
            first if first >= year.artifacts => {
                format!("Unearthed {found}, all before anyone else.")
            }
            1 => format!("Unearthed {found}, one before anyone else."),
            first => format!(
                "Unearthed {found}, {} of them before anyone else.",
                format_grouped(first)
            ),
        });
    } else if let Some((day, digs)) = &year.busiest_day
        && *digs >= 5
    {
        dig.lines
            .push(format!("Busiest day: {digs} digs on {day}."));
    }
    cards.push(dig);

    if let Some(boss) = year.bosses.first()
        && let Some(name) = (context.boss_name)(&boss.boss_id)
    {
        let mut card = ChapterCard::new(
            "story_dig_boss",
            CardStyle::Spotlight,
            mood,
            "Boss you fought most",
        );
        card.art = if boss.wins == 0 {
            CardArt::BossDefeat(boss.boss_id.clone())
        } else if boss.wins >= boss.losses {
            CardArt::BossVictory(boss.boss_id.clone())
        } else {
            CardArt::BossEncounter(boss.boss_id.clone())
        };
        card.headline = name;
        card.stat_value = plural(boss.fights(), "fight", "fights");
        card.stat_label = if boss.wins == 0 {
            "You lost every fight.".to_owned()
        } else if boss.losses == 0 {
            "You won every fight.".to_owned()
        } else {
            format!(
                "Won: {}. Lost: {}.",
                format_grouped(boss.wins),
                format_grouped(boss.losses)
            )
        };
        let others = year.bosses.len() - 1;
        if others > 0 {
            let won = year.bosses.iter().map(|boss| boss.wins).sum::<i64>();
            card.lines.push(format!(
                "Other bosses fought: {}. Total boss wins: {}.",
                format_grouped(others as i64),
                format_grouped(won)
            ));
        }
        cards.push(card);
    }

    let target = year
        .favorite_target
        .filter(|tally| tally.count >= 2)
        .and_then(|tally| Some((names(tally.discord_id)?, tally.count)));
    if year.sabotages_sent + year.sabotages_received >= 3 {
        let mut card = ChapterCard::new(
            "story_dig_feud",
            CardStyle::BigNumber,
            CardMood::DigDeep,
            "Dig sabotage",
        );
        card.headline = match &target {
            Some((name, _)) => format!("You targeted {name} most often."),
            None if year.sabotages_sent == 0 => "You received sabotage but sent none.".to_owned(),
            None => "Your sabotage activity.".to_owned(),
        };
        card.stat_value = format_grouped(year.sabotages_sent);
        card.stat_label = match &target {
            Some((name, count)) => {
                format!(
                    "sabotages sent, {} of them at {name}",
                    format_grouped(*count)
                )
            }
            None => "sabotages sent".to_owned(),
        };
        if let Some((name, count)) = year
            .top_saboteur
            .and_then(|tally| Some((names(tally.discord_id)?, tally.count)))
        {
            card.lines.push(format!(
                "{name} sabotaged you {}.",
                plural(count, "time", "times")
            ));
        }
        if year.helps_given > 0 {
            card.lines.push(format!(
                "You helped others {}.",
                plural(year.helps_given, "time", "times")
            ));
        }
        cards.push(card);
    }
    cards
}

fn cause_of_death(cause: Option<&str>) -> &'static str {
    match cause {
        Some("starvation" | "starved" | "hunger") => "starved",
        Some("eaten") => "got eaten",
        Some("brawl") => "fell in a brawl",
        _ => "died",
    }
}

#[must_use]
pub fn pet_cards(year: &PetYear, names: NameLookup<'_>, seed: u64) -> Vec<ChapterCard> {
    let Some(star) = year
        .pets
        .iter()
        .rev()
        .find(|pet| pet.is_active)
        .or_else(|| year.pets.iter().max_by_key(|pet| pet.times_fed))
    else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    let mut pet = ChapterCard::new(
        "story_pet",
        CardStyle::Spotlight,
        CardMood::Pet,
        "Camagotchi",
    );
    if star.is_active {
        pet.art = CardArt::ActivePet;
    }
    pet.headline = star.name.clone();
    // Spending is this year's; feedings count each pet's whole life.
    let across = match year.pets.len() {
        1 => String::new(),
        count => format!(" across {count} pets"),
    };
    if year.jc_spent > 0 {
        pet.stat_value = format!("{} JC", format_grouped(year.jc_spent));
        pet.stat_label = format!("on pet care{across} this year");
    } else {
        pet.stat_value = plural(year.lifetime_meals(), "meal", "meals");
        pet.stat_label = format!("served{across}, lifetime");
    }
    pet.badge = rank_badge(year.rank, "Spent more on pets than", "pet parent");
    pet.lines.push(side_joke("pet", side_tier(year.rank), seed));
    let brawls = year.brawl_wins + year.brawl_losses + year.brawl_draws;
    if brawls > 0 {
        let mut line = format!(
            "Brawls: {} won, {} lost",
            format_grouped(year.brawl_wins),
            format_grouped(year.brawl_losses)
        );
        if let Some(rival) = year
            .brawl_rival
            .filter(|tally| tally.count >= 2)
            .and_then(|tally| names(tally.discord_id))
        {
            line.push_str(&format!(". Rival: {rival}'s pet"));
        }
        line.push('.');
        pet.lines.push(line);
    }
    if year.keepsakes > 0 {
        pet.lines.push(format!(
            "Brought you {}.",
            plural(year.keepsakes, "keepsake", "keepsakes")
        ));
    }
    cards.push(pet);

    let losses = year.losses();
    if !losses.is_empty() {
        let mut grave = ChapterCard::new(
            "story_pet_grave",
            CardStyle::BigNumber,
            CardMood::PetGrave,
            "Pets lost",
        );
        grave.headline = side_joke("pet_lost", Tier::Notable, seed);
        grave.lines.push(grave.headline.clone());
        grave.stat_value = format_grouped(losses.len() as i64);
        grave.stat_label = if losses.len() == 1 {
            "pet lost this year".to_owned()
        } else {
            "pets lost this year".to_owned()
        };
        grave.lines.push(
            losses
                .iter()
                .take(3)
                .map(|pet| {
                    format!(
                        "{} {}.",
                        pet.name,
                        cause_of_death(pet.death_cause.as_deref())
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        );
        cards.push(grave);
    }
    cards
}

#[must_use]
pub fn mafia_cards(year: &MafiaYear, names: NameLookup<'_>, seed: u64) -> Vec<ChapterCard> {
    if year.games < 2 {
        return Vec::new();
    }
    let mut cards = Vec::new();
    let mut card = ChapterCard::new("story_mafia", CardStyle::Tiles, CardMood::Mafia, "Mafia");
    card.headline = if year.mafia_wins > 0 {
        pick(
            seed,
            6,
            &["You won games as Mafia.", "You had wins on the Mafia side."],
        )
    } else if year.mafia_games > 0 {
        "You played as Mafia but had no Mafia wins."
    } else if year.wins * 2 >= year.games {
        "You won at least half your games."
    } else {
        "You won fewer than half your games."
    }
    .to_owned();
    card.lines
        .push(side_joke("mafia", side_tier(year.rank), seed));
    let mut tiles = vec![
        (format_grouped(year.games), "games played".to_owned()),
        (format_grouped(year.wins), "wins".to_owned()),
    ];
    for (count, label) in [
        (year.mafia_games, "games as Mafia"),
        (year.kills, "night kills"),
        (year.saves, "lives saved"),
        (year.correct_reads, "Mafia caught"),
        (year.mvps, "MVP awards"),
        (year.jester_wins, "jester wins"),
    ] {
        if count > 0 {
            tiles.push((format_grouped(count), label.to_owned()));
        }
    }
    tiles.truncate(6);
    card.rows = tiles;
    if year.votes_cast >= 3 {
        let percent = year.votes_on_mafia as f64 / year.votes_cast as f64 * 100.0;
        card.lines
            .push(format!("{percent:.0}% of your votes targeted Mafia."));
    }
    if year.first_night_deaths >= 2 {
        card.lines.push(format!(
            "Killed on the first night {}.",
            plural(year.first_night_deaths, "time", "times")
        ));
    }
    cards.push(card);

    if let Some((name, count)) = year
        .favorite_victim
        .filter(|tally| tally.count >= 2)
        .and_then(|tally| Some((names(tally.discord_id)?, tally.count)))
    {
        let mut victim = ChapterCard::new(
            "story_mafia_victim",
            CardStyle::BigNumber,
            CardMood::MafiaRed,
            "Your most frequent target",
        );
        victim.headline = format!("You targeted {name} most often.");
        victim.stat_value = format_grouped(count);
        victim.stat_label = format!("night attacks against {name}");
        cards.push(victim);
    } else if let Some((name, count)) = year
        .top_accuser
        .filter(|tally| tally.count >= 2)
        .and_then(|tally| Some((names(tally.discord_id)?, tally.count)))
    {
        let mut accuser = ChapterCard::new(
            "story_mafia_accuser",
            CardStyle::BigNumber,
            CardMood::MafiaRed,
            "Votes against you",
        );
        accuser.headline = format!("{name} voted against you most often.");
        accuser.stat_value = format_grouped(count);
        accuser.stat_label = format!("votes from {name}");
        if year.mafia_games > 0 {
            accuser
                .lines
                .push("You also played games as Mafia.".to_owned());
        }
        cards.push(accuser);
    }
    cards
}

/// One activity's claim on the player's identity.
#[derive(Clone, Debug, PartialEq)]
pub struct PersonaSignal {
    pub activity: PersonaActivity,
    /// How strongly this activity defined the year, 0 to 100.
    pub strength: f64,
    /// The loudest number behind it, such as `("1,284", "blocks dug")`.
    pub evidence: (String, String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersonaActivity {
    /// The match queue, with the playstyle it showed.
    Queue(QueueStyle),
    Dig,
    Pets,
    GambaUp,
    GambaDown,
    MafiaLiar,
    MafiaTown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueueStyle {
    Enabler,
    Headliner,
    Mapmaker,
    Loyalist,
    Sampler,
    Unknown,
}

impl PersonaActivity {
    const fn summary(self) -> &'static str {
        match self {
            Self::Queue(_) => "Dota regular",
            Self::Dig => "Dig regular",
            Self::Pets => "Pet keeper",
            Self::GambaUp | Self::GambaDown => "Gamba regular",
            Self::MafiaLiar | Self::MafiaTown => "Mafia regular",
        }
    }

    const fn secondary_summary(self) -> &'static str {
        match self {
            Self::Queue(_) => "You also played Dota.",
            Self::Dig => "You also played Dig.",
            Self::Pets => "You also cared for pets.",
            Self::GambaUp | Self::GambaDown => "You also played Gamba.",
            Self::MafiaLiar | Self::MafiaTown => "You also played Mafia.",
        }
    }

    const fn same_family(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Queue(_), Self::Queue(_))
                | (Self::Dig, Self::Dig)
                | (Self::Pets, Self::Pets)
                | (
                    Self::GambaUp | Self::GambaDown,
                    Self::GambaUp | Self::GambaDown
                )
                | (
                    Self::MafiaLiar | Self::MafiaTown,
                    Self::MafiaLiar | Self::MafiaTown
                )
        )
    }
}

/// Summarizes the strongest activity and a runner-up from another activity.
/// Ties break toward the order the signals were given.
#[must_use]
pub fn persona_card(signals: &[PersonaSignal]) -> Option<ChapterCard> {
    let mut ranked = signals
        .iter()
        .filter(|signal| signal.strength.is_finite() && signal.strength > 0.0)
        .enumerate()
        .collect::<Vec<_>>();
    ranked.sort_by(|(a_index, a), (b_index, b)| {
        b.strength
            .total_cmp(&a.strength)
            .then_with(|| a_index.cmp(b_index))
    });
    let primary = ranked.first()?.1;
    let secondary = ranked
        .iter()
        .skip(1)
        .map(|(_, signal)| *signal)
        .find(|signal| !signal.activity.same_family(primary.activity));
    let mut card = ChapterCard::new(
        "story_persona",
        CardStyle::Persona,
        CardMood::Persona,
        "Your main activity",
    );
    card.stat_value = primary.activity.summary().to_owned();
    card.headline = secondary
        .map_or("Your main recorded activity this year.", |second| {
            second.activity.secondary_summary()
        })
        .to_owned();
    card.rows = std::iter::once(primary)
        .chain(secondary)
        .chain(ranked.iter().map(|(_, signal)| *signal).filter(|signal| {
            *signal != primary && secondary.is_none_or(|second| *signal != second)
        }))
        .take(3)
        .map(|signal| signal.evidence.clone())
        .collect();
    Some(card)
}

#[cfg(test)]
#[path = "chapters/tests.rs"]
mod tests;
