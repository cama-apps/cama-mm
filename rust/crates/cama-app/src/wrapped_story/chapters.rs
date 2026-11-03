//! Cards for the parts of a year that happen outside the match queue: the
//! dig, the Camagotchi, gamba, and Mafia, plus the month-by-month arc and the
//! "Cama type" that sums the whole year up.
//!
//! Every card is backed by a recorded number. Copy is chosen from the numbers
//! and a per-player seed, so a story reads the same every time it is replayed.

use cama_domain::wrapped_activity::{
    DigYear, GambaYear, GambleKind, MafiaYear, PetYear, ServerRank,
};

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

#[must_use]
pub fn thousands(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut output = String::with_capacity(digits.len() + 4);
    if value < 0 {
        output.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(digit);
    }
    output
}

#[must_use]
pub fn signed(value: i64) -> String {
    if value > 0 {
        format!("+{}", thousands(value))
    } else {
        thousands(value)
    }
}

fn plural(count: i64, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        thousands(count),
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
        "Your year in queue",
    );
    card.headline = pick(
        seed,
        1,
        &[
            "{month} had you in a chokehold.",
            "{month} was your peak era.",
            "You were unstoppable in {month}.",
        ],
    )
    .replace("{month}", MONTHS[peak]);
    card.stat_value = thousands(shown[peak]);
    card.stat_label = if shown[quiet] == 0 {
        format!(
            "games in {}. Then {} happened: zero games.",
            MONTHS[peak], MONTHS[quiet]
        )
    } else if quiet != peak && shown[quiet] * 3 <= shown[peak] {
        format!(
            "games in {}. {} was suspiciously quiet.",
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
                "The house would like a word.",
                "Somehow, profitable.",
                "You beat the house. Don't tell it.",
            ],
        )
    } else if summary.net < 0 {
        pick(
            seed,
            2,
            &[
                "The house sends its regards.",
                "Every bet was a lesson.",
                "You funded the house this year.",
            ],
        )
    } else {
        "Perfectly balanced, as all things should be."
    }
    .to_owned();
    curve.stat_value = format!("{} JC", signed(summary.net));
    curve.stat_label = format!("across {}", plural(summary.count, "gamble", "gambles"));
    curve.badge = rank_badge(year.rank, "Wagered more than", "bettor");
    if summary.peak > 0 && summary.net < summary.peak {
        curve.lines.push(format!(
            "Peaked at {} JC. You kept going.",
            signed(summary.peak)
        ));
    } else if summary.trough < 0 && summary.net > 0 {
        curve.lines.push(format!(
            "Down {} JC at one point. Never in doubt.",
            thousands(-summary.trough)
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
        "Degen report",
    );
    report.headline = degen
        .map(|(_, title)| title.to_owned())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "The receipts".to_owned());
    if let Some((score, _)) = degen {
        report
            .rows
            .push((score.to_string(), "degen score (lifetime)".to_owned()));
    }
    if summary.best > 0 {
        report
            .rows
            .push((signed(summary.best), "best single gamble".to_owned()));
    }
    if summary.worst < 0 {
        report
            .rows
            .push((thousands(summary.worst), "worst single gamble".to_owned()));
    }
    if year.wheel.spins > 0 {
        report
            .rows
            .push((thousands(year.wheel.spins), "wheel spins".to_owned()));
    }
    if summary.max_leverage > 1 {
        report.rows.push((
            format!("{}x", summary.max_leverage),
            "highest leverage".to_owned(),
        ));
    }
    if summary.double_or_nothing > 0 {
        report.rows.push((
            thousands(summary.double_or_nothing),
            "double or nothings".to_owned(),
        ));
    }
    if bankruptcies > 0 {
        report.rows.push((
            thousands(bankruptcies),
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
            "The wheel bankrupted you {}. You kept spinning.",
            plural(year.wheel.bankrupts, "time", "times")
        ));
    } else if summary.losing_streak >= 5 {
        report.lines.push(format!(
            "You lost {} in a row once. And then you bet again.",
            summary.losing_streak
        ));
    }
    if let Some(friend) = year
        .tips
        .top_recipient
        .and_then(|tally| Some((names(tally.discord_id)?, tally.count)))
    {
        report.lines.push(format!(
            "Your favorite charity: {}, with {} JC in tips.",
            friend.0,
            thousands(friend.1)
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
        (thousands(year.blocks_dug), "blocks dug")
    } else {
        (thousands(year.digs), "digs")
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
    dig.headline = pick(
        seed,
        3,
        &[
            "You went looking for trouble.",
            "The surface was never enough.",
            "Down was the only direction.",
        ],
    )
    .to_owned();
    let (value, _) = dig_headline_number(year);
    dig.stat_value = value;
    dig.stat_label = if year.blocks_dug > 0 {
        match context.deepest_layer {
            Some(layer) => format!(
                "blocks down. Deepest: {} in {layer}.",
                thousands(year.deepest)
            ),
            None => format!("blocks down. Deepest: {}.", thousands(year.deepest)),
        }
    } else {
        "digs".to_owned()
    };
    dig.badge = rank_badge(year.rank, "You out-dug", "digger");
    if year.cave_ins >= 3 {
        dig.lines.push(format!(
            "The tunnel caved in on you {}. You kept digging.",
            plural(year.cave_ins, "time", "times")
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
                thousands(first)
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
            if boss.losses > boss.wins {
                "Your white whale"
            } else {
                "Your favorite punching bag"
            },
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
            "It won every one. It remembers you.".to_owned()
        } else if boss.losses == 0 {
            "You won every one. It would like a rematch.".to_owned()
        } else {
            format!(
                "You won {}. It won {}.",
                thousands(boss.wins),
                thousands(boss.losses)
            )
        };
        let others = year.bosses.len() - 1;
        if others > 0 {
            let won = year.bosses.iter().map(|boss| boss.wins).sum::<i64>();
            card.lines.push(format!(
                "Plus {} more. {} boss wins all year.",
                plural(others as i64, "boss", "bosses"),
                thousands(won)
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
            "Tunnel politics",
        );
        card.headline = match &target {
            Some((name, _)) => format!("{name} was in your crosshairs."),
            None if year.sabotages_sent == 0 => "You never threw the first rock.".to_owned(),
            None => "You made enemies underground.".to_owned(),
        };
        card.stat_value = thousands(year.sabotages_sent);
        card.stat_label = match &target {
            Some((name, count)) => {
                format!("sabotages sent, {} of them at {name}", thousands(*count))
            }
            None => "sabotages sent".to_owned(),
        };
        if let Some((name, count)) = year
            .top_saboteur
            .and_then(|tally| Some((names(tally.discord_id)?, tally.count)))
        {
            card.lines.push(format!(
                "{name} got you back {}.",
                plural(count, "time", "times")
            ));
        }
        if year.helps_given > 0 {
            card.lines.push(format!(
                "You also helped others {}. It's complicated.",
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
        _ => "crossed the rainbow bridge",
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
    let meals = year.meals();
    pet.stat_value = plural(meals, "meal", "meals");
    pet.stat_label = if year.pets.len() > 1 {
        format!("served across {} pets", year.pets.len())
    } else if meals == 0 {
        pick(
            seed,
            4,
            &[
                "served. It is fine. Probably.",
                "served. Someone call welfare.",
            ],
        )
        .to_owned()
    } else {
        "served, and not one thank-you".to_owned()
    };
    pet.badge = rank_badge(year.rank, "Fed more than", "pet parent");
    if year.jc_spent > 0 && pet.badge.is_empty() {
        pet.badge = format!("{} JC on pet care", thousands(year.jc_spent));
    } else if year.jc_spent > 0 {
        pet.lines
            .push(format!("{} JC on pet care.", thousands(year.jc_spent)));
    }
    let brawls = year.brawl_wins + year.brawl_losses + year.brawl_draws;
    if brawls > 0 {
        let mut line = format!(
            "Brawls: {} won, {} lost",
            thousands(year.brawl_wins),
            thousands(year.brawl_losses)
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
            "In loving memory",
        );
        grave.headline = pick(
            seed,
            5,
            &[
                "Not everyone made it.",
                "Gone, but not forgotten. Mostly.",
                "Some goodbyes were your fault.",
            ],
        )
        .to_owned();
        grave.stat_value = thousands(losses.len() as i64);
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
            &[
                "You lied to your friends. It worked.",
                "Trustworthy face. Terrible person.",
            ],
        )
    } else if year.mafia_games > 0 {
        "You lied to your friends. They noticed."
    } else if year.wins * 2 >= year.games {
        "Town's most trusted citizen."
    } else {
        "Town's most suspicious citizen."
    }
    .to_owned();
    let mut tiles = vec![
        (thousands(year.games), "games played".to_owned()),
        (thousands(year.wins), "on the winning side".to_owned()),
    ];
    for (count, label) in [
        (year.mafia_games, "times Mafia"),
        (year.kills, "night kills"),
        (year.saves, "lives saved"),
        (year.correct_reads, "Mafia caught"),
        (year.mvps, "times MVP"),
        (year.jester_wins, "jester wins"),
    ] {
        if count > 0 {
            tiles.push((thousands(count), label.to_owned()));
        }
    }
    tiles.truncate(6);
    card.rows = tiles;
    if year.votes_cast >= 3 {
        let percent = year.votes_on_mafia as f64 / year.votes_cast as f64 * 100.0;
        card.lines.push(format!(
            "{percent:.0}% of your votes landed on actual Mafia."
        ));
    }
    if year.first_night_deaths >= 2 {
        card.lines.push(format!(
            "Killed on night one {}. They saw you coming.",
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
            "Nothing personal",
        );
        victim.headline = format!("{name} should sleep with one eye open.");
        victim.stat_value = thousands(count);
        victim.stat_label = "times you came for them at night".to_owned();
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
            "Trust issues",
        );
        accuser.headline = format!("{name} never trusted you.");
        accuser.stat_value = thousands(count);
        accuser.stat_label = "votes against you".to_owned();
        if year.mafia_games > 0 {
            accuser
                .lines
                .push("To be fair, sometimes they were right.".to_owned());
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
    const fn noun(self) -> &'static str {
        match self {
            Self::Queue(QueueStyle::Enabler) => "Enabler",
            Self::Queue(QueueStyle::Headliner) => "Headliner",
            Self::Queue(QueueStyle::Mapmaker) => "Cartographer",
            Self::Queue(QueueStyle::Loyalist) => "Loyalist",
            Self::Queue(QueueStyle::Sampler) => "Sampler",
            Self::Queue(QueueStyle::Unknown) => "Queue Goblin",
            Self::Dig => "Mole",
            Self::Pets => "Pet Parent",
            Self::GambaUp => "High Roller",
            Self::GambaDown => "Degen",
            Self::MafiaLiar => "Snake",
            Self::MafiaTown => "Town Crier",
        }
    }

    const fn adjective(self) -> &'static str {
        match self {
            Self::Queue(_) => "Tryhard",
            Self::Dig => "Subterranean",
            Self::Pets => "Doting",
            Self::GambaUp => "Lucky",
            Self::GambaDown => "Unhinged",
            Self::MafiaLiar => "Two-Faced",
            Self::MafiaTown => "Paranoid",
        }
    }

    const fn clause(self) -> &'static str {
        match self {
            Self::Queue(_) => "the queue was home",
            Self::Dig => "the ground was a personal rival",
            Self::Pets => "a small creature depended on you",
            Self::GambaUp => "the house was your piggy bank",
            Self::GambaDown => "the wheel was a financial plan",
            Self::MafiaLiar => "your friends couldn't trust a word you said",
            Self::MafiaTown => "you trusted absolutely no one",
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

/// The year in one name: the strongest activity is the noun, the runner-up
/// the adjective. Ties break toward the order the signals were given.
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
    let adjective = match secondary {
        Some(signal) => signal.activity.adjective(),
        None if primary.strength >= 75.0 => "Certified",
        None => "Part-Time",
    };
    let mut card = ChapterCard::new(
        "story_persona",
        CardStyle::Persona,
        CardMood::Persona,
        "Your Cama type",
    );
    card.stat_value = format!("The {adjective} {}", primary.activity.noun());
    card.headline = match secondary {
        Some(second) => format!(
            "A year where {}, and {}.",
            primary.activity.clause(),
            second.activity.clause()
        ),
        None => format!("A year where {}.", primary.activity.clause()),
    };
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
