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
        title: "You made time",
        headline: "One more game. Several times.",
        value: format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
        label: "recorded match time",
        detail: format!(
            "{} seconds in the Ancient business. Queue time not included.",
            number(seconds)
        ),
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
                title: "You had notes",
                headline: "The minimap heard you.",
                value: number(pings),
                label: "recorded pings",
                detail: format!(
                    "{:.1} pings per recorded game. A very interactive presentation.",
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
                title: "The plot twist",
                headline: "You weren't done yet.",
                value: number(deficit as i64),
                label: "team gold deficit overcome",
                detail: "Your team won from this far behind. The comeback had an ensemble cast."
                    .to_owned(),
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
                title: "The encore",
                headline: "Winning was becoming a habit.",
                value: longest.to_string(),
                label: "wins in a row",
                detail: "Your longest recorded win streak. Obviously, you queued again.".to_owned(),
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
                title: "Delivery interrupted",
                headline: "Someone's items were late.",
                value: number(couriers),
                label: "recorded courier kills",
                detail: "You took a hands-on approach to supply-chain management.".to_owned(),
                coverage: coverage(courier_games),
            },
        );
    }
    for (key, title, headline, label, detail, threshold, metric) in [
        (
            "healing",
            "The health plan",
            "Your team had coverage.",
            "recorded hero healing",
            "Keeping the group project alive, literally.",
            8000.0,
            (|row: &PersonalMomentMatch| row.hero_healing)
                as fn(&PersonalMomentMatch) -> Option<i64>,
        ),
        (
            "wards",
            "You brought receipts",
            "The map wasn't going to light itself.",
            "recorded wards placed",
            "Observer and sentry wards. Someone had to fund the vision board.",
            10.0,
            (|row: &PersonalMomentMatch| row.wards) as fn(&PersonalMomentMatch) -> Option<i64>,
        ),
        (
            "towers",
            "Property damage",
            "You understood the assignment.",
            "recorded tower damage",
            "Ancients don't fall to a good KDA alone.",
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
                title: "The extended edition",
                headline: "That quick game had other plans.",
                value: format!("{}:{:02}", duration / 60, duration % 60),
                label: "your longest recorded game",
                detail: "Minutes and seconds. Somewhere, a bedtime was quietly abandoned."
                    .to_owned(),
                coverage: coverage(durations.len()),
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
            return Some(make(
                "The enabler",
                "You made everyone else look good.",
                format!(
                    "{assists:.1} assists per recorded game. Main-character energy, shared generously."
                ),
                combat.len(),
            ));
        }
        if kills >= 10.0 && kills > assists {
            return Some(make(
                "The headliner",
                "Your name kept making the feed.",
                format!(
                    "{kills:.1} kills per recorded game. The highlight reel had a recurring lead."
                ),
                combat.len(),
            ));
        }
    }
    let (wards, count) = observed(matches, |row| row.wards);
    if count >= 3 && wards as f64 / count as f64 >= 10.0 {
        return Some(make(
            "The mapmaker",
            "You saw the bigger picture.",
            format!(
                "{:.1} wards per recorded game. Your team had a view.",
                wards as f64 / count as f64
            ),
            count,
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
            return Some(make(
                "The loyalist",
                "You found your person. They have spells.",
                format!("One hero in {top} of {picks} recorded picks. Comfort is a strategy."),
                picks,
            ));
        }
        if heroes.len() >= 5 && heroes.len() as f64 / picks as f64 >= 0.6 {
            return Some(make(
                "The sampler",
                "The hero grid was a tasting menu.",
                format!(
                    "{} heroes across {picks} recorded picks. Commitment issues, excellent range.",
                    heroes.len()
                ),
                picks,
            ));
        }
    }
    None
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
        assert!(time.detail.contains("3,663 seconds"));
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
        assert_eq!(a.value, "The enabler");
        assert_eq!(b.value, "The headliner");
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
        assert_eq!(identity.value, "The loyalist");
        assert!(identity.detail.contains("3 of 3 recorded picks"));
        assert_eq!(identity.coverage, "Recorded in 3 of 10 games");
        assert!(personal_moments(&rows[..2]).identity.is_none());
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
        assert_eq!(identity.value, "The sampler");
        assert!(identity.detail.contains("6 heroes across 6 recorded picks"));
    }
}
