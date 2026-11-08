//! Evidence-backed, deterministic personal chapters for Wrapped.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonalMomentMatch {
    pub hero_id: Option<i64>,
    pub won: Option<bool>,
    pub duration_seconds: Option<i64>,
    pub kills: Option<i64>,
    pub deaths: Option<i64>,
    pub assists: Option<i64>,
    pub wards: Option<i64>,
    pub pings: Option<i64>,
    pub courier_kills: Option<i64>,
    pub hero_healing: Option<i64>,
    pub tower_damage: Option<i64>,
    pub comeback: Option<f64>,
    pub apm: Option<f64>,
    /// Divine Rapiers bought, when the match was enriched.
    pub rapiers: Option<i64>,
    /// The biggest team gold lead held in a game that was then lost.
    pub throw: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalMoment {
    pub key: &'static str,
    pub title: &'static str,
    pub headline: &'static str,
    pub value: String,
    pub label: &'static str,
    pub detail: String,
    pub coverage: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonalStory {
    pub identity: Option<PersonalMoment>,
    pub playtime: Option<PersonalMoment>,
    pub moments: Vec<PersonalMoment>,
}

/// Matches must be in chronological order (oldest or newest first).
#[must_use]
pub fn personal_moments(matches: &[PersonalMomentMatch]) -> PersonalStory {
    let coverage = |count| format!("Recorded in {count} of {} games", matches.len());
    let durations = matches
        .iter()
        .filter_map(|row| row.duration_seconds.filter(|value| *value > 0))
        .collect::<Vec<_>>();
    let seconds = durations.iter().sum::<i64>();
    let playtime = (!durations.is_empty()).then(|| PersonalMoment {
        key: "playtime",
        title: "Time played",
        headline: "Your time in matches.",
        value: format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
        label: "recorded match time",
        detail: "Queue time is not included.".to_owned(),
        coverage: coverage(durations.len()),
    });
    let identity = identity(matches);
    // Pick the strongest eligible story from each theme before filling spare
    // slots. A well-enriched support should not receive five support totals.
    let mut candidates = Vec::<(u8, f64, PersonalMoment)>::new();
    let mut add = |theme, score, moment| candidates.push((theme, score, moment));
    let (pings, ping_games) = observed(matches, |row| row.pings);
    if ping_games >= 3 && pings as f64 / ping_games as f64 >= 30.0 {
        add(
            0,
            pings as f64 / ping_games as f64 / 30.0,
            PersonalMoment {
                key: "pings",
                title: "Pings",
                headline: "You had a lot to say.",
                value: number(pings),
                label: "recorded pings",
                detail: format!(
                    "{:.1} pings per recorded game.",
                    pings as f64 / ping_games as f64
                ),
                coverage: coverage(ping_games),
            },
        );
    }
    let comeback_games = matches
        .iter()
        .filter(|row| {
            row.won.is_some()
                && row
                    .comeback
                    .is_some_and(|value| value.is_finite() && value >= 0.0)
        })
        .count();
    let comeback = matches
        .iter()
        .filter(|row| row.won == Some(true))
        .filter_map(|row| {
            row.comeback
                .filter(|value| value.is_finite() && *value >= 5000.0)
        })
        .max_by(f64::total_cmp);
    if let Some(deficit) = comeback {
        add(
            1,
            deficit / 5000.0,
            PersonalMoment {
                key: "comeback",
                title: "Biggest comeback",
                headline: "Your team won from behind.",
                value: number(deficit as i64),
                label: "team gold deficit overcome",
                detail: "Your largest recorded team gold deficit in a win.".to_owned(),
                coverage: coverage(comeback_games),
            },
        );
    }
    let mut streak = 0;
    let mut longest = 0;
    for row in matches {
        streak = if row.won == Some(true) { streak + 1 } else { 0 };
        longest = longest.max(streak);
    }
    if longest >= 3 {
        add(
            1,
            f64::from(longest) / 5.0,
            PersonalMoment {
                key: "streak",
                title: "Longest win streak",
                headline: "You kept winning.",
                value: longest.to_string(),
                label: "wins in a row",
                detail: "Your longest run of consecutive wins.".to_owned(),
                coverage: coverage(matches.iter().filter(|row| row.won.is_some()).count()),
            },
        );
    }
    let (couriers, courier_games) = observed(matches, |row| row.courier_kills);
    if courier_games >= 3 && couriers >= 3 {
        add(
            2,
            couriers as f64 / courier_games as f64,
            PersonalMoment {
                key: "couriers",
                title: "Courier kills",
                headline: "Someone's items were late.",
                value: number(couriers),
                label: "recorded courier kills",
                detail: "Total enemy couriers killed.".to_owned(),
                coverage: coverage(courier_games),
            },
        );
    }
    for (key, title, headline, label, detail, threshold, metric) in [
        (
            "healing",
            "Healing",
            "You kept your teammates alive.",
            "recorded hero healing",
            "Total hero healing.",
            8000.0,
            (|row: &PersonalMomentMatch| row.hero_healing)
                as fn(&PersonalMomentMatch) -> Option<i64>,
        ),
        (
            "wards",
            "Wards placed",
            "You kept the map lit.",
            "recorded wards placed",
            "Observer and sentry wards combined.",
            10.0,
            (|row: &PersonalMomentMatch| row.wards) as fn(&PersonalMomentMatch) -> Option<i64>,
        ),
        (
            "towers",
            "Tower damage",
            "You hit the towers.",
            "recorded tower damage",
            "Total damage to towers.",
            5000.0,
            (|row: &PersonalMomentMatch| row.tower_damage)
                as fn(&PersonalMomentMatch) -> Option<i64>,
        ),
    ] {
        let (total, count) = observed(matches, metric);
        let average = total as f64 / count.max(1) as f64;
        if count >= 3 && average >= threshold {
            add(
                3,
                average / threshold,
                PersonalMoment {
                    key,
                    title,
                    headline,
                    value: number(total),
                    label,
                    detail: format!("{detail} {:.0} per recorded game.", average),
                    coverage: coverage(count),
                },
            );
        }
    }
    if let Some(duration) = durations.iter().max().filter(|duration| **duration >= 3600) {
        add(
            4,
            *duration as f64 / 3600.0,
            PersonalMoment {
                key: "marathon",
                title: "Longest game",
                headline: "This one took a while.",
                value: format!("{}:{:02}", duration / 60, duration % 60),
                label: "your longest recorded game",
                detail: "Time shown in minutes and seconds.".to_owned(),
                coverage: coverage(durations.len()),
            },
        );
    }
    let apm = matches
        .iter()
        .filter_map(|row| row.apm.filter(|value| value.is_finite() && *value > 0.0))
        .collect::<Vec<_>>();
    let average_apm = apm.iter().sum::<f64>() / apm.len().max(1) as f64;
    if apm.len() >= 3 && average_apm >= 120.0 {
        add(
            5,
            average_apm / 150.0,
            PersonalMoment {
                key: "apm",
                title: "Actions per minute",
                headline: "You kept busy.",
                value: format!("{average_apm:.0}"),
                label: "actions per minute",
                detail: "Average across games with action data.".to_owned(),
                coverage: coverage(apm.len()),
            },
        );
    }
    let (rapiers, rapier_games) = observed(matches, |row| row.rapiers);
    if rapiers > 0 {
        let lost_with_rapier = matches
            .iter()
            .filter(|row| row.rapiers.is_some_and(|count| count > 0) && row.won == Some(false))
            .count();
        add(
            6,
            1.0 + rapiers as f64,
            PersonalMoment {
                key: "rapier",
                title: "Divine Rapiers",
                headline: if rapiers == 1 {
                    "You bought a Divine Rapier."
                } else {
                    "You bought Divine Rapiers."
                },
                value: number(rapiers),
                label: if rapiers == 1 {
                    "Divine Rapier"
                } else {
                    "Divine Rapiers"
                },
                detail: if lost_with_rapier > 0 {
                    "You lost at least one game after buying a Rapier.".to_owned()
                } else {
                    "No recorded losses after buying a Rapier.".to_owned()
                },
                coverage: coverage(rapier_games),
            },
        );
    }
    let throw_games = matches
        .iter()
        .filter(|row| {
            row.won.is_some()
                && row
                    .throw
                    .is_some_and(|value| value.is_finite() && value >= 0.0)
        })
        .count();
    if let Some(lead) = matches
        .iter()
        .filter(|row| row.won == Some(false))
        .filter_map(|row| {
            row.throw
                .filter(|value| value.is_finite() && *value >= 8000.0)
        })
        .max_by(f64::total_cmp)
    {
        add(
            1,
            lead / 8000.0,
            PersonalMoment {
                key: "throw",
                title: "Biggest lost lead",
                headline: "Your team lost from ahead.",
                value: number(lead as i64),
                label: "team gold lead, then a loss",
                detail: "Your largest recorded team gold lead in a loss.".to_owned(),
                coverage: coverage(throw_games),
            },
        );
    }
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.2.key.cmp(b.2.key)));
    let mut selected = Vec::new();
    let mut themes = std::collections::BTreeSet::new();
    for (index, (theme, _, _)) in candidates.iter().enumerate() {
        if themes.insert(*theme) {
            selected.push(index);
        }
    }
    for index in 0..candidates.len() {
        if selected.len() >= 5 {
            break;
        }
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    selected.truncate(5);
    let moments = selected
        .into_iter()
        .map(|index| candidates[index].2.clone())
        .collect();
    PersonalStory {
        identity,
        playtime,
        moments,
    }
}

/// Personal awards from distinct themes, independent of match ordering.
/// Unknown or invalid measurements never count as zero or as a known outcome.
#[must_use]
pub fn personal_superlatives(matches: &[PersonalMomentMatch]) -> Vec<PersonalMoment> {
    let coverage = |count| format!("Recorded in {count} of {} games", matches.len());
    // Each candidate describes a different achievement. Rank by strength relative
    // to its eligibility threshold; stable keys settle equally strong stories.
    let mut candidates = Vec::<(f64, PersonalMoment)>::new();
    let survival = matches
        .iter()
        .filter_map(|row| Some((row.won?, row.deaths.filter(|value| *value >= 0)?)))
        .collect::<Vec<_>>();
    let deathless = survival
        .iter()
        .filter(|(won, deaths)| *won && *deaths == 0)
        .count();
    if deathless > 0 {
        candidates.push((
            1.0 + deathless as f64 / survival.len() as f64,
            PersonalMoment {
                key: "deathless",
                title: "Deathless wins",
                headline: "You won without dying.",
                value: deathless.to_string(),
                label: "wins with zero deaths",
                detail: "You stayed alive for the whole game.".to_owned(),
                coverage: coverage(survival.len()),
            },
        ));
    }
    let assists = matches
        .iter()
        .filter_map(|row| row.assists.filter(|value| *value >= 0))
        .collect::<Vec<_>>();
    if let Some(peak) = assists.iter().max().filter(|value| **value >= 20) {
        candidates.push((
            *peak as f64 / 20.0,
            PersonalMoment {
                key: "supporting_cast",
                title: "Most assists",
                headline: "Your best game for assists.",
                value: number(*peak),
                label: "assists in one game",
                detail: "Your highest recorded assist total.".to_owned(),
                coverage: coverage(assists.len()),
            },
        ));
    }
    let timed_results = matches
        .iter()
        .filter_map(|row| Some((row.won?, row.duration_seconds.filter(|value| *value > 0)?)))
        .collect::<Vec<_>>();
    if let Some(duration) = timed_results
        .iter()
        .filter(|(won, _)| *won)
        .map(|(_, duration)| *duration)
        .min()
        .filter(|duration| *duration <= 1800)
    {
        candidates.push((
            1800.0 / duration as f64,
            PersonalMoment {
                key: "speedrun",
                title: "Fastest win",
                headline: "You made it quick.",
                value: format!("{}:{:02}", duration / 60, duration % 60),
                label: "minutes : seconds",
                detail: "Time shown in minutes and seconds.".to_owned(),
                coverage: coverage(timed_results.len()),
            },
        ));
    }
    let scorelines = matches
        .iter()
        .filter_map(|row| {
            Some((
                row.won?,
                row.kills.filter(|value| *value >= 0)?,
                row.deaths.filter(|value| *value >= 0)?,
                row.assists.filter(|value| *value >= 0)?,
            ))
        })
        .collect::<Vec<_>>();
    if let Some((_, kills, deaths, assists)) = scorelines
        .iter()
        .filter(|(won, kills, _, _)| !*won && *kills >= 15)
        .max_by_key(|(_, kills, deaths, assists)| (*kills, std::cmp::Reverse(*deaths), *assists))
    {
        candidates.push((
            *kills as f64 / 15.0,
            PersonalMoment {
                key: "bittersweet",
                title: "Most kills in a loss",
                headline: "All those kills. Still a loss.",
                value: format!("{kills} / {deaths} / {assists}"),
                label: "kills / deaths / assists in a loss",
                detail: "Your highest-kill loss with complete combat stats.".to_owned(),
                coverage: coverage(scorelines.len()),
            },
        ));
    }
    let heroes = matches
        .iter()
        .filter_map(|row| row.hero_id.filter(|value| *value > 0))
        .collect::<Vec<_>>();
    let unique = heroes
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if unique >= 5 && unique as f64 / heroes.len() as f64 >= 0.6 {
        candidates.push((
            unique as f64 / 5.0,
            PersonalMoment {
                key: "hero_sampler",
                title: "Hero variety",
                headline: "You tried plenty of heroes.",
                value: unique.to_string(),
                label: "different heroes played",
                detail: format!("{unique} heroes across {} recorded picks.", heroes.len()),
                coverage: coverage(heroes.len()),
            },
        ));
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.key.cmp(b.1.key)));
    candidates
        .into_iter()
        .take(3)
        .map(|(_, moment)| moment)
        .collect()
}

fn observed(
    matches: &[PersonalMomentMatch],
    metric: impl Fn(&PersonalMomentMatch) -> Option<i64>,
) -> (i64, usize) {
    matches
        .iter()
        .filter_map(metric)
        .filter(|value| *value >= 0)
        .fold((0, 0), |(sum, count), value| (sum + value, count + 1))
}

fn identity(matches: &[PersonalMomentMatch]) -> Option<PersonalMoment> {
    let make = |value: &str, headline, detail, count| PersonalMoment {
        key: "identity",
        title: "Your playstyle",
        headline,
        value: value.to_owned(),
        label: "your style in the recorded games",
        detail,
        coverage: format!("Recorded in {count} of {} games", matches.len()),
    };
    let mut candidates = Vec::<(f64, PersonalMoment)>::new();
    let combat = matches
        .iter()
        .filter_map(|row| {
            Some((
                row.kills.filter(|value| *value >= 0)?,
                row.assists.filter(|value| *value >= 0)?,
            ))
        })
        .collect::<Vec<_>>();
    if combat.len() >= 3 {
        let kills = combat.iter().map(|row| row.0).sum::<i64>() as f64 / combat.len() as f64;
        let assists = combat.iter().map(|row| row.1).sum::<i64>() as f64 / combat.len() as f64;
        if assists >= 15.0 && assists > kills * 1.5 {
            candidates.push((
                assists / 15.0,
                make(
                    "Team player",
                    "You helped your teammates get kills.",
                    format!("{assists:.1} assists per recorded game."),
                    combat.len(),
                ),
            ));
        }
        if kills >= 10.0 && kills > assists {
            candidates.push((
                kills / 10.0,
                make(
                    "High kills",
                    "You got plenty of kills.",
                    format!("{kills:.1} kills per recorded game."),
                    combat.len(),
                ),
            ));
        }
    }
    let (wards, count) = observed(matches, |row| row.wards);
    if count >= 3 && wards as f64 / count as f64 >= 10.0 {
        candidates.push((
            wards as f64 / count as f64 / 10.0,
            make(
                "Wards",
                "You placed plenty of wards.",
                format!(
                    "{:.1} wards per recorded game.",
                    wards as f64 / count as f64
                ),
                count,
            ),
        ));
    }
    let mut heroes = std::collections::BTreeMap::new();
    for hero in matches
        .iter()
        .filter_map(|row| row.hero_id.filter(|value| *value > 0))
    {
        *heroes.entry(hero).or_insert(0_usize) += 1;
    }
    let picks = heroes.values().sum::<usize>();
    if picks >= 3 {
        let top = heroes.values().copied().max().unwrap_or_default();
        if top >= 3 && top as f64 / picks as f64 >= 0.6 {
            candidates.push((
                top as f64 / picks as f64 / 0.6,
                make(
                    "Hero specialist",
                    "You stuck with your favorite.",
                    format!("One hero in {top} of {picks} recorded picks."),
                    picks,
                ),
            ));
        }
        if heroes.len() >= 5 && heroes.len() as f64 / picks as f64 >= 0.6 {
            candidates.push((
                heroes.len() as f64 / picks as f64 / 0.6,
                make(
                    "Hero variety",
                    "You played a wide range of heroes.",
                    format!("{} heroes across {picks} recorded picks.", heroes.len()),
                    picks,
                ),
            ));
        }
    }
    // Keep ties stable when display names change.
    let tie_order = [
        "Team player",
        "High kills",
        "Hero specialist",
        "Wards",
        "Hero variety",
    ];
    candidates.sort_by(|a, b| {
        b.0.total_cmp(&a.0).then_with(|| {
            let rank =
                |moment: &PersonalMoment| tie_order.iter().position(|name| *name == moment.value);
            rank(&a.1).cmp(&rank(&b.1))
        })
    });
    candidates.into_iter().next().map(|(_, moment)| moment)
}

fn number(value: i64) -> String {
    let digits = value.to_string();
    let mut output = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(digit);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apm_rapiers_and_throws_need_recorded_evidence() {
        let mut rows = vec![
            PersonalMomentMatch {
                apm: Some(180.0),
                rapiers: Some(0),
                won: Some(true),
                throw: Some(2_000.0),
                ..Default::default()
            };
            3
        ];
        rows[1].rapiers = Some(2);
        rows[1].won = Some(false);
        rows[1].throw = Some(12_500.0);
        let story = personal_moments(&rows);
        let key = |key| story.moments.iter().find(|moment| moment.key == key);
        let apm = key("apm").expect("fast hands");
        assert_eq!(apm.value, "180");
        assert_eq!(apm.coverage, "Recorded in 3 of 3 games");
        let rapier = key("rapier").expect("rapier");
        assert_eq!(rapier.value, "2");
        assert!(rapier.detail.contains("lost at least one game"));
        let throw = key("throw").expect("thrown lead");
        assert_eq!(throw.value, "12,500");

        let quiet = vec![
            PersonalMomentMatch {
                apm: Some(60.0),
                won: Some(false),
                throw: Some(12_500.0),
                ..Default::default()
            };
            2
        ];
        let story = personal_moments(&quiet);
        assert!(story.moments.iter().all(|moment| moment.key != "apm"));
        assert!(story.moments.iter().all(|moment| moment.key != "rapier"));
        assert!(story.moments.iter().any(|moment| moment.key == "throw"));
        let won_anyway = vec![
            PersonalMomentMatch {
                won: Some(true),
                throw: Some(30_000.0),
                ..Default::default()
            };
            3
        ];
        assert!(
            personal_moments(&won_anyway)
                .moments
                .iter()
                .all(|moment| moment.key != "throw"),
            "a lead that was kept is not a throw"
        );
    }

    #[test]
    fn missing_enrichment_does_not_invent_habits_or_identity() {
        assert_eq!(personal_moments(&[]), PersonalStory::default());
        let rows = vec![PersonalMomentMatch::default(); 12];
        assert_eq!(personal_moments(&rows), PersonalStory::default());
    }

    #[test]
    fn recorded_time_sums_actual_positive_durations_and_discloses_coverage() {
        let rows = [
            PersonalMomentMatch {
                duration_seconds: Some(3601),
                ..Default::default()
            },
            PersonalMomentMatch {
                duration_seconds: Some(62),
                ..Default::default()
            },
            PersonalMomentMatch {
                duration_seconds: Some(0),
                ..Default::default()
            },
            PersonalMomentMatch {
                duration_seconds: Some(-5),
                ..Default::default()
            },
            PersonalMomentMatch::default(),
        ];
        let time = personal_moments(&rows).playtime.expect("recorded time");
        assert_eq!(time.value, "1h 1m");
        assert_eq!(time.coverage, "Recorded in 2 of 5 games");
    }

    #[test]
    fn contrasting_profiles_receive_different_evidence_backed_identities() {
        let support = vec![
            PersonalMomentMatch {
                kills: Some(3),
                assists: Some(22),
                wards: Some(14),
                ..Default::default()
            };
            6
        ];
        let carry = vec![
            PersonalMomentMatch {
                kills: Some(16),
                assists: Some(5),
                deaths: Some(4),
                ..Default::default()
            };
            6
        ];
        let a = personal_moments(&support)
            .identity
            .expect("support identity");
        let b = personal_moments(&carry).identity.expect("carry identity");
        assert_eq!(a.value, "Team player");
        assert_eq!(b.value, "High kills");
        assert!(a.detail.contains("22.0 assists"));
        assert!(b.detail.contains("16.0 kills"));
        assert_eq!(a.coverage, "Recorded in 6 of 6 games");
    }

    #[test]
    fn hero_identity_requires_enough_known_picks_and_uses_known_denominator() {
        let mut rows = vec![
            PersonalMomentMatch {
                hero_id: Some(7),
                ..Default::default()
            };
            3
        ];
        rows.extend(vec![PersonalMomentMatch::default(); 7]);
        let identity = personal_moments(&rows).identity.expect("repeat hero");
        assert_eq!(identity.value, "Hero specialist");
        assert!(identity.detail.contains("3 of 3 recorded picks"));
        assert_eq!(identity.coverage, "Recorded in 3 of 10 games");
        assert!(personal_moments(&rows[..2]).identity.is_none());
    }

    #[test]
    fn renamed_playstyles_preserve_the_existing_tie_order() {
        let rows = (0..5)
            .map(|index| PersonalMomentMatch {
                hero_id: Some(if index < 3 { 7 } else { 8 }),
                wards: Some(10),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let identity = personal_moments(&rows).identity.unwrap();
        assert!(identity.detail.starts_with("One hero in 3 of 5"));
    }

    #[test]
    fn unknown_result_breaks_streak_and_losing_comeback_is_not_a_win() {
        let mut rows = vec![
            PersonalMomentMatch {
                won: Some(true),
                ..Default::default()
            };
            2
        ];
        rows.push(PersonalMomentMatch {
            won: None,
            ..Default::default()
        });
        rows.extend(vec![
            PersonalMomentMatch {
                won: Some(true),
                ..Default::default()
            };
            2
        ]);
        rows.push(PersonalMomentMatch {
            won: Some(false),
            comeback: Some(20000.0),
            ..Default::default()
        });
        assert!(personal_moments(&rows).moments.is_empty());
        rows.push(PersonalMomentMatch {
            won: Some(true),
            comeback: Some(8000.0),
            ..Default::default()
        });
        let moments = personal_moments(&rows).moments;
        assert_eq!(moments.len(), 1);
        assert_eq!(moments[0].key, "comeback");
        assert_eq!(moments[0].value, "8,000");
    }

    #[test]
    fn selections_are_deterministic_bounded_and_varied() {
        let rows = vec![
            PersonalMomentMatch {
                won: Some(true),
                duration_seconds: Some(4800),
                kills: Some(4),
                assists: Some(20),
                wards: Some(16),
                pings: Some(90),
                courier_kills: Some(2),
                hero_healing: Some(22000),
                tower_damage: Some(15000),
                comeback: Some(9000.0),
                ..Default::default()
            };
            8
        ];
        let story = personal_moments(&rows);
        assert_eq!(story, personal_moments(&rows));
        assert_eq!(story.moments.len(), 5);
        let keys = story
            .moments
            .iter()
            .map(|moment| moment.key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys.len(), 5);
        assert!(keys.contains("pings"));
        assert!(keys.contains("comeback"));
        assert!(
            story
                .moments
                .iter()
                .all(|moment| !moment.detail.is_empty() && !moment.coverage.is_empty())
        );
    }

    #[test]
    fn small_samples_and_non_finite_metrics_do_not_create_roasts() {
        let rows = vec![
            PersonalMomentMatch {
                pings: Some(500),
                wards: Some(100),
                comeback: Some(f64::NAN),
                ..Default::default()
            };
            2
        ];
        let story = personal_moments(&rows);
        assert!(story.identity.is_none());
        assert!(story.moments.is_empty());
    }

    #[test]
    fn lower_activity_does_not_trigger_high_activity_copy() {
        let rows = vec![
            PersonalMomentMatch {
                pings: Some(29),
                wards: Some(9),
                courier_kills: Some(0),
                hero_healing: Some(7999),
                tower_damage: Some(4999),
                duration_seconds: Some(3599),
                ..Default::default()
            };
            3
        ];
        let story = personal_moments(&rows);
        assert!(story.moments.is_empty());
        assert!(story.identity.is_none());
    }

    #[test]
    fn a_broad_hero_pool_receives_a_distinct_identity() {
        let rows = (1..=6)
            .map(|hero| PersonalMomentMatch {
                hero_id: Some(hero),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let identity = personal_moments(&rows).identity.expect("variety identity");
        assert_eq!(identity.value, "Hero variety");
        assert!(identity.detail.contains("6 heroes across 6 recorded picks"));
    }
}

#[cfg(test)]
mod encore_tests {
    use super::*;

    #[test]
    fn deathless_award_counts_only_known_wins_and_deaths() {
        let rows = [
            PersonalMomentMatch {
                won: Some(true),
                deaths: Some(0),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(false),
                deaths: Some(0),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(true),
                deaths: None,
                ..Default::default()
            },
            PersonalMomentMatch {
                won: None,
                deaths: Some(0),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(true),
                deaths: Some(-1),
                ..Default::default()
            },
        ];
        let awards = personal_superlatives(&rows);
        assert_eq!(awards.len(), 1);
        assert_eq!(awards[0].key, "deathless");
        assert_eq!(awards[0].value, "1");
        assert_eq!(awards[0].coverage, "Recorded in 2 of 5 games");
        assert!(personal_superlatives(&rows[1..]).is_empty());
    }

    #[test]
    fn assists_award_reports_peak_and_known_coverage() {
        let rows = [
            PersonalMomentMatch {
                assists: Some(25),
                ..Default::default()
            },
            PersonalMomentMatch {
                assists: Some(32),
                ..Default::default()
            },
            PersonalMomentMatch {
                assists: Some(-1),
                ..Default::default()
            },
            PersonalMomentMatch::default(),
        ];
        let awards = personal_superlatives(&rows);
        assert_eq!(awards[0].key, "supporting_cast");
        assert_eq!(awards[0].value, "32");
        assert_eq!(awards[0].coverage, "Recorded in 2 of 4 games");
    }

    #[test]
    fn fastest_win_ignores_losses_unknown_results_and_invalid_durations() {
        let rows = [
            PersonalMomentMatch {
                won: Some(true),
                duration_seconds: Some(1261),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(true),
                duration_seconds: Some(2000),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(false),
                duration_seconds: Some(400),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: None,
                duration_seconds: Some(300),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(true),
                duration_seconds: Some(0),
                ..Default::default()
            },
        ];
        let awards = personal_superlatives(&rows);
        assert_eq!(awards[0].key, "speedrun");
        assert_eq!(awards[0].value, "21:01");
        assert_eq!(awards[0].coverage, "Recorded in 3 of 5 games");
    }

    #[test]
    fn bittersweet_award_uses_a_complete_recorded_score_in_a_loss() {
        let rows = [
            PersonalMomentMatch {
                won: Some(false),
                kills: Some(24),
                deaths: Some(4),
                assists: Some(8),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(true),
                kills: Some(30),
                deaths: Some(1),
                assists: Some(10),
                ..Default::default()
            },
            PersonalMomentMatch {
                won: Some(false),
                kills: Some(40),
                ..Default::default()
            },
        ];
        let awards = personal_superlatives(&rows);
        assert_eq!(awards[0].key, "bittersweet");
        assert_eq!(awards[0].value, "24 / 4 / 8");
        assert_eq!(awards[0].coverage, "Recorded in 2 of 3 games");
    }

    #[test]
    fn variety_award_counts_distinct_known_heroes_and_does_not_reward_one_tricking() {
        let mut rows = (1..=6)
            .map(|hero| PersonalMomentMatch {
                hero_id: Some(hero),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        rows.push(PersonalMomentMatch::default());
        let awards = personal_superlatives(&rows);
        assert_eq!(awards[0].key, "hero_sampler");
        assert_eq!(awards[0].value, "6");
        assert_eq!(awards[0].coverage, "Recorded in 6 of 7 games");
        assert!(personal_superlatives(&vec![rows[0].clone(); 10]).is_empty());
    }

    #[test]
    fn superlatives_are_bounded_unique_and_independent_of_input_order() {
        let mut rows = (1..=6)
            .map(|hero| PersonalMomentMatch {
                hero_id: Some(hero),
                won: Some(true),
                deaths: Some(0),
                assists: Some(30),
                duration_seconds: Some(1200),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let awards = personal_superlatives(&rows);
        assert_eq!(awards.len(), 3);
        assert_eq!(
            awards
                .iter()
                .map(|a| a.key)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        rows.reverse();
        assert_eq!(awards, personal_superlatives(&rows));
        let moments = personal_moments(&rows).moments;
        assert!(
            awards
                .iter()
                .all(|award| moments.iter().all(|moment| moment.key != award.key))
        );
        assert!(personal_superlatives(&vec![PersonalMomentMatch::default(); 20]).is_empty());
    }

    #[test]
    fn identity_compares_strength_instead_of_always_choosing_combat() {
        let rows = vec![
            PersonalMomentMatch {
                kills: Some(3),
                assists: Some(16),
                wards: Some(30),
                ..Default::default()
            };
            6
        ];
        let identity = personal_moments(&rows)
            .identity
            .expect("strong vision identity");
        assert_eq!(identity.value, "Wards");
        assert!(identity.detail.contains("30.0 wards"));
    }
}
