use std::collections::BTreeMap;

use super::*;

fn amounts(distributions: &[Distribution]) -> BTreeMap<DiscordId, i64> {
    distributions
        .iter()
        .map(|distribution| (distribution.discord_id, distribution.amount))
        .collect()
}

#[test]
fn test_even_distribution_basic() {
    let split = calculate_even_distribution(
        200,
        &[
            Debtor {
                discord_id: 1001,
                balance: -100,
            },
            Debtor {
                discord_id: 1002,
                balance: -100,
            },
        ],
    );
    assert_eq!(amounts(&split), BTreeMap::from([(1001, 100), (1002, 100)]));

    let capped = calculate_even_distribution(
        200,
        &[
            Debtor {
                discord_id: 1001,
                balance: -10,
            },
            Debtor {
                discord_id: 1002,
                balance: -500,
            },
        ],
    );
    assert_eq!(amounts(&capped), BTreeMap::from([(1001, 10), (1002, 190)]));

    let debt_limited = calculate_even_distribution(
        100,
        &[
            Debtor {
                discord_id: 1001,
                balance: -30,
            },
            Debtor {
                discord_id: 1002,
                balance: -20,
            },
        ],
    );
    assert_eq!(
        amounts(&debt_limited),
        BTreeMap::from([(1001, 30), (1002, 20)])
    );

    let small_debts: Vec<_> = (1..=10)
        .map(|discord_id| Debtor {
            discord_id,
            balance: -5,
        })
        .collect();
    let many = calculate_even_distribution(100, &small_debts);
    assert_eq!(many.iter().map(|entry| entry.amount).sum::<i64>(), 50);
    assert!(many.iter().all(|entry| entry.amount == 5));
}

#[test]
fn test_proportional_distribution_capped() {
    let proportional = calculate_proportional_distribution(
        100,
        &[
            Debtor {
                discord_id: 1001,
                balance: -300,
            },
            Debtor {
                discord_id: 1002,
                balance: -200,
            },
        ],
    );
    let proportional_amounts = amounts(&proportional);
    assert!(proportional_amounts[&1001] >= 55);
    assert!(proportional_amounts[&1002] >= 35);
    assert_eq!(proportional_amounts.values().sum::<i64>(), 100);

    let capped = calculate_proportional_distribution(
        100,
        &[
            Debtor {
                discord_id: 1001,
                balance: -10,
            },
            Debtor {
                discord_id: 1002,
                balance: -1000,
            },
        ],
    );
    let capped_amounts = amounts(&capped);
    assert_eq!(capped_amounts.values().sum::<i64>(), 100);
    assert_eq!(capped_amounts[&1001], 1);
    assert_eq!(capped_amounts[&1002], 99);
    assert!(capped_amounts[&1001] <= 10);
}

#[test]
fn test_neediest_distribution_basic() {
    let distributions = calculate_neediest_distribution(
        200,
        &[
            Debtor {
                discord_id: 1001,
                balance: -100,
            },
            Debtor {
                discord_id: 1002,
                balance: -500,
            },
            Debtor {
                discord_id: 1003,
                balance: -50,
            },
        ],
    );
    assert_eq!(distributions, vec![Distribution::new(1002, 200)]);
    assert_eq!(
        calculate_neediest_distribution(
            200,
            &[Debtor {
                discord_id: 1001,
                balance: -50,
            }]
        ),
        vec![Distribution::new(1001, 50)]
    );
}

#[test]
fn test_stimulus_distribution_basic() {
    let four = [
        StimulusCandidate {
            discord_id: 1001,
            balance: 50,
        },
        StimulusCandidate {
            discord_id: 1002,
            balance: 40,
        },
        StimulusCandidate {
            discord_id: 1003,
            balance: 30,
        },
        StimulusCandidate {
            discord_id: 1004,
            balance: 20,
        },
    ];
    let distributions = calculate_stimulus_distribution(100, &four);
    assert_eq!(
        distributions.iter().map(|entry| entry.amount).sum::<i64>(),
        100
    );
    assert!(distributions.iter().all(|entry| entry.amount == 25));

    let three = &four[..3];
    assert_eq!(
        amounts(&calculate_stimulus_distribution(100, three)),
        BTreeMap::from([(1001, 34), (1002, 33), (1003, 33)])
    );
    assert!(calculate_stimulus_distribution(100, &[]).is_empty());
    assert_eq!(
        calculate_stimulus_distribution(100, &four[..1]),
        vec![Distribution::new(1001, 100)]
    );
}

#[test]
fn test_lottery_distribution_basic() {
    let players = [1001, 1002, 1003];
    let distributions = calculate_lottery_distribution(100, &players, 4);
    assert_eq!(distributions.len(), 1);
    assert_eq!(distributions[0].amount, 100);
    assert!(players.contains(&distributions[0].discord_id));
    assert_eq!(
        calculate_lottery_distribution(100, &[1001], 999),
        vec![Distribution::new(1001, 100)]
    );
    assert!(calculate_lottery_distribution(100, &[], 0).is_empty());
}

#[test]
fn test_social_security_distribution_basic() {
    let players = [
        GamesCandidate {
            discord_id: 1001,
            games_played: 30,
        },
        GamesCandidate {
            discord_id: 1002,
            games_played: 50,
        },
        GamesCandidate {
            discord_id: 1003,
            games_played: 20,
        },
    ];
    let distributions = calculate_social_security_distribution(100, &players);
    let by_player = amounts(&distributions);
    assert_eq!(by_player.values().sum::<i64>(), 100);
    assert!(by_player[&1002] >= by_player[&1001]);
    assert!(by_player[&1002] >= by_player[&1003]);
    assert_eq!(
        calculate_social_security_distribution(100, &players[..1]),
        vec![Distribution::new(1001, 100)]
    );
    let equal = [
        GamesCandidate {
            discord_id: 1001,
            games_played: 10,
        },
        GamesCandidate {
            discord_id: 1002,
            games_played: 10,
        },
    ];
    assert_eq!(
        calculate_social_security_distribution(100, &equal)
            .iter()
            .map(|entry| entry.amount)
            .sum::<i64>(),
        100
    );
    assert!(calculate_social_security_distribution(100, &[]).is_empty());
}

#[test]
fn test_social_security_distribution_zero_games() {
    let players = [
        GamesCandidate {
            discord_id: 1001,
            games_played: 0,
        },
        GamesCandidate {
            discord_id: 1002,
            games_played: 0,
        },
    ];
    assert!(calculate_social_security_distribution(100, &players).is_empty());
}

#[test]
fn test_richest_distribution_basic() {
    assert_eq!(
        calculate_richest_distribution(
            100,
            Some(RichestCandidate {
                discord_id: 1001,
                balance: 500,
            })
        ),
        vec![Distribution::new(1001, 100)]
    );
    assert_eq!(
        calculate_richest_distribution(
            500,
            Some(RichestCandidate {
                discord_id: 1001,
                balance: 10,
            })
        ),
        vec![Distribution::new(1001, 500)]
    );
    assert!(calculate_richest_distribution(100, None).is_empty());
}

fn snapshot(discord_id: DiscordId, balance: i64, games: u32) -> PlayerSnapshot {
    PlayerSnapshot {
        discord_id,
        username: format!("player{discord_id}"),
        balance,
        wins: games,
        losses: 0,
        last_match_at: None,
    }
}

#[test]
fn test_distribution_method_from_code_round_trips() {
    for method in DistributionMethod::ALL {
        assert_eq!(DistributionMethod::from_code(method.as_str()), Some(method));
    }
    assert_eq!(DistributionMethod::from_code("unknown"), None);
    assert_eq!(DistributionMethod::from_code("EVEN"), None);
}

#[test]
fn test_select_debtors_orders_most_indebted_first() {
    let roster = [
        snapshot(1, -50, 1),
        snapshot(2, -500, 1),
        snapshot(3, 100, 1),
        snapshot(4, -50, 0),
    ];
    assert_eq!(
        select_debtors(&roster),
        vec![
            Debtor {
                discord_id: 2,
                balance: -500,
            },
            Debtor {
                discord_id: 1,
                balance: -50,
            },
            Debtor {
                discord_id: 4,
                balance: -50,
            },
        ]
    );
}

#[test]
fn test_method_distributions_debtor_methods_target_debtors() {
    let roster = [
        snapshot(1, -100, 5),
        snapshot(2, -300, 5),
        snapshot(3, 900, 5),
    ];
    assert_eq!(
        amounts(&calculate_method_distributions(
            DistributionMethod::Even,
            200,
            &roster,
            None
        )),
        BTreeMap::from([(1, 100), (2, 100)])
    );
    assert_eq!(
        amounts(&calculate_method_distributions(
            DistributionMethod::Proportional,
            100,
            &roster,
            None
        )),
        BTreeMap::from([(1, 25), (2, 75)])
    );
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Neediest, 150, &roster, None),
        vec![Distribution::new(2, 150)]
    );
}

#[test]
fn test_method_distributions_even_remainder_goes_to_the_most_indebted() {
    // Remainder coins follow the (balance asc, discord id asc) debtor order,
    // deliberately superseding the legacy unspecified roster order.
    let roster = [snapshot(1, -5, 1), snapshot(2, -9, 1)];
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Even, 1, &roster, None),
        vec![Distribution::new(2, 1)]
    );
}

#[test]
fn test_method_distributions_stimulus_skips_the_three_richest_players() {
    let roster = [
        snapshot(1, 900, 2),
        snapshot(2, 800, 2),
        snapshot(3, 700, 2),
        snapshot(4, 600, 2),
        snapshot(5, 500, 2),
        // Unplayed accounts never displace a real player from the top three.
        snapshot(6, 5000, 0),
    ];
    assert_eq!(
        amounts(&calculate_method_distributions(
            DistributionMethod::Stimulus,
            100,
            &roster,
            None
        )),
        BTreeMap::from([(4, 50), (5, 50)])
    );
}

#[test]
fn test_method_distributions_social_security_excludes_richest_non_debtors() {
    let roster = [
        snapshot(1, 900, 10),
        snapshot(2, 800, 10),
        snapshot(3, 700, 10),
        snapshot(4, 50, 6),
        snapshot(5, -500, 4),
    ];
    // Debtors are never part of the richest exclusion, so 4 and 5 split the
    // fund 6:4 by games played.
    assert_eq!(
        calculate_method_distributions(DistributionMethod::SocialSecurity, 100, &roster, None),
        vec![Distribution::new(4, 60), Distribution::new(5, 40)]
    );
}

#[test]
fn test_method_distributions_lottery_pays_only_the_drawn_winner() {
    let roster = [snapshot(1, 10, 1), snapshot(2, 20, 1)];
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Lottery, 250, &roster, Some(2)),
        vec![Distribution::new(2, 250)]
    );
    assert!(
        calculate_method_distributions(DistributionMethod::Lottery, 250, &roster, None).is_empty()
    );
}

#[test]
fn test_method_distributions_richest_breaks_ties_on_the_lowest_id() {
    let roster = [snapshot(7, 250, 1), snapshot(3, 250, 1)];
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Richest, 90, &roster, None),
        vec![Distribution::new(3, 90)]
    );
}

#[test]
fn test_method_distributions_non_paying_methods_and_empty_fund() {
    let roster = [snapshot(1, -100, 1)];
    for method in [
        DistributionMethod::Burn,
        DistributionMethod::NextMatchPot,
        DistributionMethod::Cancel,
    ] {
        assert!(calculate_method_distributions(method, 100, &roster, None).is_empty());
    }
    assert!(calculate_method_distributions(DistributionMethod::Even, 0, &roster, None).is_empty());
}
