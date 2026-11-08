use cama_domain::wrapped_activity::{
    BossRecord, GambleEvent, PersonTally, PetRecord, TipYear, WheelYear,
};

use super::*;

const SEED: u64 = 7;

fn nobody(_: i64) -> Option<String> {
    None
}

fn everyone(id: i64) -> Option<String> {
    Some(format!("Player {id}"))
}

fn bet(at: i64, profit: i64) -> GambleEvent {
    GambleEvent {
        at,
        profit,
        kind: GambleKind::Bet,
        exposure: profit.abs(),
        leverage: 1,
    }
}

#[test]
fn copy_choices_are_stable_per_player_and_vary_across_players() {
    let pool = ["a", "b", "c", "d"];
    assert_eq!(pick(SEED, 1, &pool), pick(SEED, 1, &pool));
    let distinct = (0..64)
        .map(|seed| pick(story_seed(seed, 2026), 1, &pool))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(distinct.len() > 1);
}

#[test]
fn months_card_finds_the_peak_and_the_quiet_month() {
    let mut dates = vec!["2026-01-04 10:00:00"; 3];
    dates.extend(vec!["2026-03-04 10:00:00"; 9]);
    dates.push("2026-05-01 00:00:00");
    dates.push("garbage");
    let card = months_card(&dates, SEED).expect("two active months");
    assert_eq!(card.style, CardStyle::Bars);
    assert!(card.headline.contains("March"));
    assert_eq!(card.stat_value, "9");
    assert!(card.stat_label.contains("February"), "{}", card.stat_label);
    assert_eq!(card.series, vec![3.0, 0.0, 9.0, 0.0, 1.0]);
    assert_eq!(
        card.rows.len(),
        5,
        "months after the last game are left off"
    );
    assert!(months_card(&["2026-01-01"; 20], SEED).is_none());
    assert!(months_card(&[], SEED).is_none());
}

#[test]
fn gamba_chapter_needs_a_real_habit() {
    let year = GambaYear {
        events: vec![bet(1, 10), bet(2, -5)],
        ..GambaYear::default()
    };
    assert!(gamba_cards(&year, None, 0, &nobody, SEED).is_empty());
}

#[test]
fn gamba_curve_and_report_tell_the_true_story() {
    let mut events = vec![bet(1, 300), bet(2, 200)];
    for at in 3..10 {
        events.push(bet(at, -150));
    }
    events.push(GambleEvent {
        at: 11,
        profit: -40,
        kind: GambleKind::Wheel,
        exposure: 40,
        leverage: 1,
    });
    events[0].leverage = 5;
    let year = GambaYear {
        events,
        wheel: WheelYear {
            spins: 1,
            bankrupts: 1,
            ..WheelYear::default()
        },
        tips: TipYear {
            top_recipient: Some(PersonTally {
                discord_id: 9,
                count: 120,
            }),
            ..TipYear::default()
        },
        rank: Some(ServerRank { rank: 1, of: 8 }),
    };
    let cards = gamba_cards(&year, Some((73, "Certified Degen")), 2, &everyone, SEED);
    let [curve, report] = cards.as_slice() else {
        panic!("curve and report: {cards:?}");
    };
    assert_eq!(curve.mood, CardMood::GambaDown);
    assert_eq!(curve.stat_value, "-590 JC");
    assert_eq!(curve.stat_label, "across 10 gambles");
    assert_eq!(curve.badge, "#1 bettor on the server");
    assert_eq!(curve.lines, ["Highest net profit: +500 JC."]);
    assert_eq!(curve.series.first(), Some(&0.0));
    assert_eq!(curve.series.last(), Some(&-590.0));
    assert_eq!(report.headline, "Your biggest wins and losses.");
    let labels = report
        .rows
        .iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        [
            "degen score (lifetime)",
            "biggest win (JC)",
            "biggest loss (JC)",
            "wheel spins",
            "highest leverage",
            "bankruptcies, lifetime"
        ]
    );
    assert!(report.lines[0].contains("bankrupted you 1 time"));
    assert!(report.lines[1].contains("Player 9"));

    let anonymous = gamba_cards(&year, None, 0, &nobody, SEED);
    assert!(
        anonymous
            .iter()
            .flat_map(|card| &card.lines)
            .all(|line| !line.contains("You tipped")),
        "an unresolvable name is left out, never shown as an ID"
    );
}

fn boss_name(id: &str) -> Option<String> {
    (id == "grothak").then(|| "Grothak the Unyielding".to_owned())
}

#[test]
fn dig_chapter_spotlights_the_most_fought_boss_and_feuds() {
    let year = DigYear {
        digs: 40,
        blocks_dug: 1_284,
        deepest: 412,
        cave_ins: 37,
        artifacts: 3,
        first_finds: 1,
        bosses: vec![
            BossRecord {
                boss_id: "grothak".into(),
                wins: 1,
                losses: 6,
            },
            BossRecord {
                boss_id: "lina".into(),
                wins: 2,
                losses: 0,
            },
        ],
        sabotages_sent: 5,
        sabotages_received: 2,
        favorite_target: Some(PersonTally {
            discord_id: 3,
            count: 4,
        }),
        top_saboteur: Some(PersonTally {
            discord_id: 4,
            count: 2,
        }),
        rank: Some(ServerRank { rank: 2, of: 14 }),
        ..DigYear::default()
    };
    let context = DigContext {
        boss_name: &boss_name,
        deepest_layer: Some("Magma"),
        deep: true,
    };
    let cards = dig_cards(&year, &context, &everyone, SEED);
    let kinds = cards.iter().map(|card| card.kind).collect::<Vec<_>>();
    assert_eq!(kinds, ["story_dig", "story_dig_boss", "story_dig_feud"]);
    let dig = &cards[0];
    assert_eq!(dig.mood, CardMood::DigDeep);
    assert_eq!(dig.stat_value, "1,284");
    assert_eq!(dig.stat_label, "blocks dug. Deepest: 412 in Magma.");
    assert_eq!(dig.badge, "Dug more than 92% of the server");
    assert!(dig.lines[0].contains("37 cave-ins"));
    assert!(dig.lines[1].contains("3 artifacts, one before anyone else"));
    let boss = &cards[1];
    assert_eq!(boss.art, CardArt::BossEncounter("grothak".into()));
    assert_eq!(boss.title, "Boss you fought most");
    assert_eq!(boss.stat_value, "7 fights");
    assert_eq!(boss.stat_label, "Won: 1. Lost: 6.");
    assert_eq!(boss.lines, ["Other bosses fought: 1. Total boss wins: 3."]);
    let feud = &cards[2];
    assert_eq!(feud.headline, "You targeted Player 3 most often.");
    assert_eq!(feud.lines, ["Player 4 sabotaged you 2 times."]);

    let unnamed = dig_cards(&year, &context, &nobody, SEED);
    assert_eq!(unnamed[2].headline, "Your sabotage activity.");
    let unknown_boss = DigContext {
        boss_name: &|_| None,
        ..context
    };
    assert!(
        dig_cards(&year, &unknown_boss, &everyone, SEED)
            .iter()
            .all(|card| card.kind != "story_dig_boss"),
        "a boss without a readable name gets no spotlight"
    );
    assert!(
        dig_cards(
            &DigYear {
                digs: 4,
                ..year.clone()
            },
            &context,
            &everyone,
            SEED
        )
        .is_empty()
    );
}

#[test]
fn a_year_of_cave_ins_counts_digs_not_imaginary_blocks() {
    let year = DigYear {
        digs: 12,
        blocks_dug: 0,
        rank: Some(ServerRank { rank: 1, of: 3 }),
        ..DigYear::default()
    };
    let context = DigContext {
        boss_name: &boss_name,
        deepest_layer: None,
        deep: false,
    };
    let dig = &dig_cards(&year, &context, &nobody, SEED)[0];
    assert_eq!(
        (dig.stat_value.as_str(), dig.stat_label.as_str()),
        ("12", "digs")
    );
    assert_eq!(dig.badge, "#1 digger on the server");
    assert_eq!(dig_headline_number(&year), ("12".to_owned(), "digs"));
}

#[test]
fn first_finds_read_naturally_however_many_there_were() {
    let line = |artifacts, first_finds| {
        let year = DigYear {
            digs: 5,
            blocks_dug: 20,
            artifacts,
            first_finds,
            ..DigYear::default()
        };
        let context = DigContext {
            boss_name: &boss_name,
            deepest_layer: None,
            deep: false,
        };
        dig_cards(&year, &context, &nobody, SEED)[0]
            .lines
            .iter()
            .find(|line| line.starts_with("Unearthed"))
            .cloned()
            .unwrap()
    };
    assert_eq!(line(2, 0), "Unearthed 2 artifacts.");
    assert_eq!(line(1, 1), "Unearthed 1 artifact before anyone else.");
    assert_eq!(line(3, 3), "Unearthed 3 artifacts, all before anyone else.");
    assert_eq!(line(3, 1), "Unearthed 3 artifacts, one before anyone else.");
    assert_eq!(
        line(9, 4),
        "Unearthed 9 artifacts, 4 of them before anyone else."
    );
}

#[test]
fn undefeated_and_winless_boss_records_read_correctly() {
    let context = DigContext {
        boss_name: &boss_name,
        deepest_layer: None,
        deep: false,
    };
    for (wins, losses, art, label) in [
        (
            0,
            3,
            CardArt::BossDefeat("grothak".into()),
            "You lost every fight.",
        ),
        (
            4,
            0,
            CardArt::BossVictory("grothak".into()),
            "You won every fight.",
        ),
    ] {
        let year = DigYear {
            digs: 9,
            blocks_dug: 20,
            bosses: vec![BossRecord {
                boss_id: "grothak".into(),
                wins,
                losses,
            }],
            ..DigYear::default()
        };
        let cards = dig_cards(&year, &context, &nobody, SEED);
        assert_eq!(cards[0].mood, CardMood::Dig);
        assert_eq!(cards[1].art, art);
        assert!(cards[1].stat_label.starts_with(label));
    }
}

fn pet(name: &str, fed: i64, active: bool, death: Option<&str>) -> PetRecord {
    PetRecord {
        name: name.to_owned(),
        times_fed: fed,
        is_active: active,
        died_at: death.map(|_| 5),
        death_cause: death.map(str::to_owned),
        ..PetRecord::default()
    }
}

#[test]
fn pet_chapter_stars_the_living_pet_and_mourns_the_rest() {
    let year = PetYear {
        pets: vec![
            pet("Old Timer", 200, false, Some("starvation")),
            pet("Biscuit", 30, false, Some("eaten")),
            pet("Traded", 9, false, Some("sacrifice")),
            pet("Sir Fluff", 182, true, None),
        ],
        jc_spent: 4_120,
        brawl_wins: 12,
        brawl_losses: 3,
        brawl_rival: Some(PersonTally {
            discord_id: 5,
            count: 6,
        }),
        keepsakes: 2,
        ..PetYear::default()
    };
    let cards = pet_cards(&year, &everyone, SEED);
    let [star, grave] = cards.as_slice() else {
        panic!("star and grave: {cards:?}");
    };
    assert_eq!(star.art, CardArt::ActivePet);
    assert_eq!(star.headline, "Sir Fluff");
    assert_eq!(star.stat_value, "421 meals");
    assert_eq!(star.stat_label, "served across 4 pets");
    assert_eq!(star.badge, "4,120 JC on pet care");
    assert_eq!(
        star.lines,
        [
            "Brawls: 12 won, 3 lost. Rival: Player 5's pet.",
            "Brought you 2 keepsakes."
        ]
    );
    assert_eq!(grave.stat_value, "2");
    assert_eq!(grave.lines, ["Old Timer starved. Biscuit got eaten."]);

    assert!(pet_cards(&PetYear::default(), &everyone, SEED).is_empty());
    let lonely = PetYear {
        pets: vec![pet("Ghost", 3, false, Some("starvation"))],
        ..PetYear::default()
    };
    let cards = pet_cards(&lonely, &everyone, SEED);
    assert_eq!(cards[0].art, CardArt::None, "no living pet, no portrait");
    assert_eq!(cards[0].headline, "Ghost");
}

#[test]
fn mafia_chapter_tiles_only_what_happened() {
    let year = MafiaYear {
        games: 34,
        wins: 20,
        mafia_games: 9,
        mafia_wins: 7,
        kills: 6,
        mvps: 3,
        votes_cast: 10,
        votes_on_mafia: 4,
        favorite_victim: Some(PersonTally {
            discord_id: 8,
            count: 6,
        }),
        ..MafiaYear::default()
    };
    let cards = mafia_cards(&year, &everyone, SEED);
    let [tiles, victim] = cards.as_slice() else {
        panic!("tiles and victim: {cards:?}");
    };
    let labels = tiles
        .rows
        .iter()
        .map(|(_, label)| label.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        [
            "games played",
            "wins",
            "games as Mafia",
            "night kills",
            "MVP awards"
        ]
    );
    assert_eq!(tiles.lines, ["40% of your votes targeted Mafia."]);
    assert_eq!(victim.headline, "You targeted Player 8 most often.");
    assert_eq!(victim.stat_value, "6");

    let honest = MafiaYear {
        games: 5,
        wins: 1,
        top_accuser: Some(PersonTally {
            discord_id: 2,
            count: 3,
        }),
        ..MafiaYear::default()
    };
    let cards = mafia_cards(&honest, &everyone, SEED);
    assert_eq!(cards[0].headline, "You won fewer than half your games.");
    assert_eq!(cards[1].kind, "story_mafia_accuser");
    assert_eq!(cards[1].headline, "Player 2 voted against you most often.");
    assert!(mafia_cards(&honest, &nobody, SEED).len() == 1);
    assert!(
        mafia_cards(
            &MafiaYear {
                games: 1,
                ..MafiaYear::default()
            },
            &everyone,
            SEED
        )
        .is_empty()
    );
}

fn signal(activity: PersonaActivity, strength: f64, value: &str) -> PersonaSignal {
    PersonaSignal {
        activity,
        strength,
        evidence: (value.to_owned(), "proof".to_owned()),
    }
}

#[test]
fn persona_names_the_strongest_activity_and_its_runner_up() {
    let card = persona_card(&[
        signal(PersonaActivity::Queue(QueueStyle::Mapmaker), 40.0, "q"),
        signal(PersonaActivity::Dig, 92.0, "d"),
        signal(PersonaActivity::GambaDown, 80.0, "g"),
        signal(PersonaActivity::Pets, 10.0, "p"),
    ])
    .expect("persona");
    assert_eq!(card.stat_value, "Dig regular");
    assert_eq!(card.headline, "You also played Gamba.");
    let evidence = card
        .rows
        .iter()
        .map(|(value, _)| value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(evidence, ["d", "g", "q"]);
}

#[test]
fn persona_skips_same_family_runner_ups_and_handles_one_activity() {
    let card = persona_card(&[
        signal(PersonaActivity::Queue(QueueStyle::Enabler), 90.0, "q"),
        signal(PersonaActivity::Queue(QueueStyle::Enabler), 80.0, "q2"),
    ])
    .unwrap();
    assert_eq!(card.stat_value, "Dota regular");
    assert_eq!(card.headline, "Your main recorded activity this year.");
    let casual = persona_card(&[signal(PersonaActivity::Pets, 20.0, "p")]).unwrap();
    assert_eq!(casual.stat_value, "Pet keeper");
    assert!(persona_card(&[]).is_none());
    assert!(persona_card(&[signal(PersonaActivity::Dig, f64::NAN, "d")]).is_none());
}
